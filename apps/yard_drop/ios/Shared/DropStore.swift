import Darwin
import Foundation

enum DropError: LocalizedError {
  case invalidCapture, expiredCapture, invalidBatch, limit, cancelled

  var errorDescription: String? {
    switch self {
    case .invalidCapture: return "Reserve this iPhone in YARD to send files to the browser."
    case .expiredCapture:
      return "The YARD connection expired. Reconnect the iPhone to its provider and share again."
    case .invalidBatch: return "The shared files are incomplete. Share them again."
    case .limit: return "Share up to 20 files, 512 MB per file and 2 GB in total."
    case .cancelled: return "The share was cancelled."
    }
  }
}

struct DropCapture: Codable, Equatable {
  let id: String
  let expiresAt: Int64
}

struct DropFile: Codable {
  let id: String
  let name: String
  let typeIdentifier: String
  let mimeType: String
  let size: Int64
  let stagedName: String
}

struct DropProducer: Codable {
  let appVersion: String
  let buildNumber: Int
  let commit: String

  static var current: Self {
    Self(
      appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        ?? "dev",
      buildNumber: Int(
        Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0") ?? 0,
      commit: Bundle.main.object(forInfoDictionaryKey: "YardCommit") as? String ?? "local"
    )
  }
}

struct DropBatch: Codable {
  let schemaVersion: Int
  let batchId: String
  var captureId: String?
  let createdAt: Int64
  let producer: DropProducer
  let files: [DropFile]
}

final class DropStore {
  static var groupID: String? {
    Bundle.main.object(forInfoDictionaryKey: "YardAppGroup") as? String
  }
  static let marker = "_YARD_READY"
  static let maxFile: Int64 = 512 * 1024 * 1024
  static let maxBatch: Int64 = 2 * 1024 * 1024 * 1024
  static let captureLifetime: Int64 = 10 * 60 * 1000
  let root: URL
  private let fm = FileManager.default

  init(root: URL) { self.root = root }

  static func shared() throws -> DropStore {
    guard
      let groupID,
      let group = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: groupID)
    else { throw DropError.invalidCapture }
    return DropStore(
      root: group.appendingPathComponent("Library/Caches/YARD Drop")
    )
  }

  static var now: Int64 { Int64(Date().timeIntervalSince1970 * 1000) }
  static func validID(_ value: String) -> Bool { UUID(uuidString: value) != nil }

  static func safeName(_ name: String) -> String {
    let invalid = CharacterSet.controlCharacters.union(CharacterSet(charactersIn: "/\\:"))
    var clean = name.components(separatedBy: invalid).joined(separator: "_")
      .trimmingCharacters(in: .whitespacesAndNewlines)
    while clean.utf8.count > 180 { clean.removeLast() }
    return clean.isEmpty || clean == "." || clean == ".." ? "attachment" : clean
  }

  static func uniqueName(_ original: String, used: inout Set<String>) -> String {
    let safe = safeName(original)
    let path = safe as NSString
    let ext = path.pathExtension
    let stem = path.deletingPathExtension
    var name = safe
    var suffix = 1
    while !used.insert(name.lowercased()).inserted {
      name = "\(stem) (\(suffix))" + (ext.isEmpty ? "" : ".\(ext)")
      suffix += 1
    }
    return name
  }

  // Serialise extension instances while committing complete batches.
  private func locked<T>(_ action: () throws -> T) throws -> T {
    try fm.createDirectory(at: root, withIntermediateDirectories: true)
    let descriptor = open(root.appendingPathComponent(".lock").path, O_CREAT | O_RDWR, 0o600)
    guard descriptor >= 0 else { throw DropError.invalidBatch }
    defer { close(descriptor) }
    guard flock(descriptor, LOCK_EX) == 0 else { throw DropError.invalidBatch }
    defer { flock(descriptor, LOCK_UN) }
    return try action()
  }

  private var captureURL: URL { root.appendingPathComponent("active-capture.json") }
  private var incoming: URL { root.appendingPathComponent("Incoming") }

  private func readCapture(now: Int64) throws -> DropCapture {
    guard let data = try? Data(contentsOf: captureURL),
      let capture = try? JSONDecoder().decode(DropCapture.self, from: data),
      Self.validID(capture.id)
    else { throw DropError.invalidCapture }
    guard capture.expiresAt > now, capture.expiresAt <= now + Self.captureLifetime
    else { throw DropError.expiredCapture }
    return capture
  }

  func activeCapture(now: Int64 = DropStore.now) -> DropCapture? {
    try? locked { try readCapture(now: now) }
  }

  func makeStagingDirectory() throws -> URL {
    try locked {
      let exports = root.appendingPathComponent("Exports")
      try fm.createDirectory(at: exports, withIntermediateDirectories: true)
      for url in try fm.contentsOfDirectory(
        at: exports, includingPropertiesForKeys: [.contentModificationDateKey])
      {
        let date =
          try url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate
          ?? .distantPast
        if date < Date().addingTimeInterval(-24 * 60 * 60) { try removeIfPresent(url) }
      }
      let url = exports.appendingPathComponent(UUID().uuidString)
      try fm.createDirectory(at: url, withIntermediateDirectories: true)
      return url
    }
  }

  func queue(_ batch: DropBatch, staged: URL, capture: DropCapture, now: Int64 = DropStore.now)
    throws
  {
    try locked {
      guard try readCapture(now: now).id == capture.id else { throw DropError.invalidCapture }
      var queued = batch
      queued.captureId = capture.id
      try JSONEncoder().encode(queued).write(
        to: staged.appendingPathComponent("batch.json"), options: .atomic)
      let parent = incoming.appendingPathComponent(capture.id)
      try fm.createDirectory(at: parent, withIntermediateDirectories: true)
      let destination = parent.appendingPathComponent(batch.batchId)
      try Data().write(to: staged.appendingPathComponent(Self.marker), options: .atomic)
      try fm.moveItem(at: staged, to: destination)
    }
  }

  static func copy(
    _ source: URL, to destination: URL, remaining: Int64, cancelled: () -> Bool = { false }
  ) throws -> Int64 {
    guard let input = InputStream(url: source),
      let output = OutputStream(url: destination, append: false)
    else { throw DropError.invalidBatch }
    input.open()
    output.open()
    defer {
      input.close()
      output.close()
    }
    var success = false
    defer { if !success { try? FileManager.default.removeItem(at: destination) } }
    let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: 64 * 1024)
    defer { buffer.deallocate() }
    var size: Int64 = 0
    while true {
      if cancelled() { throw DropError.cancelled }
      let count = input.read(buffer, maxLength: 64 * 1024)
      guard count >= 0 else { throw input.streamError ?? DropError.invalidBatch }
      if count == 0 { break }
      size += Int64(count)
      guard size <= maxFile, size <= remaining else { throw DropError.limit }
      var offset = 0
      while offset < count {
        let written = output.write(buffer.advanced(by: offset), maxLength: count - offset)
        guard written > 0 else { throw output.streamError ?? DropError.invalidBatch }
        offset += written
      }
    }
    success = true
    return size
  }

  private func removeIfPresent(_ url: URL) throws {
    if fm.fileExists(atPath: url.path) { try fm.removeItem(at: url) }
  }
}
