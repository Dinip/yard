import Darwin
import Foundation

enum DropError: LocalizedError {
  case invalidCapture, expiredCapture, invalidBatch, limit, cancelled

  var errorDescription: String? {
    switch self {
    case .invalidCapture: return "Open Receive shared files in YARD and prepare the iPhone first."
    case .expiredCapture: return "This transfer expired. Prepare the iPhone again in YARD."
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

enum DropCommand {
  case arm(DropCapture)
  case publish(String)

  init?(arguments: [String]) throws {
    guard let index = arguments.firstIndex(of: "--yard-drop") else { return nil }
    let values = Array(arguments.dropFirst(index + 1))
    guard values.count >= 2, DropStore.validID(values[1]) else { throw DropError.invalidCapture }
    switch values[0] {
    case "arm":
      guard values.count == 3, let expiry = Int64(values[2]) else { throw DropError.invalidCapture }
      self = .arm(DropCapture(id: values[1], expiresAt: expiry))
    case "publish":
      guard values.count == 2 else { throw DropError.invalidCapture }
      self = .publish(values[1])
    default: throw DropError.invalidCapture
    }
  }
}

final class DropStore {
  static var groupID: String {
    Bundle.main.object(forInfoDictionaryKey: "YardAppGroup") as? String
      ?? "group.com.dinispimpao.yard.drop"
  }
  static let marker = "_YARD_READY"
  static let maxFile: Int64 = 512 * 1024 * 1024
  static let maxBatch: Int64 = 2 * 1024 * 1024 * 1024
  static let captureLifetime: Int64 = 10 * 60 * 1000
  let root: URL
  let documents: URL
  private let fm = FileManager.default

  init(root: URL, documents: URL) {
    self.root = root
    self.documents = documents
  }

  static func shared() throws -> DropStore {
    guard
      let group = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: groupID)
    else { throw DropError.invalidCapture }
    return DropStore(
      root: group.appendingPathComponent("Library/Caches/YARD Drop"),
      documents: FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
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

  // The extension and Runner are separate processes; arm must not race a queued batch.
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
    try? locked {
      do { return try readCapture(now: now) } catch DropError.expiredCapture {
        try removeIfPresent(incoming)
        try removeIfPresent(captureURL)
        return nil
      }
    }
  }

  func arm(_ capture: DropCapture, now: Int64 = DropStore.now) throws {
    try locked {
      guard Self.validID(capture.id) else { throw DropError.invalidCapture }
      guard capture.expiresAt > now, capture.expiresAt <= now + Self.captureLifetime
      else { throw DropError.expiredCapture }
      try removeIfPresent(incoming)
      try removeIfPresent(captureURL)
      // A new capture cannot offer the previous reservation's published files.
      try removeIfPresent(documents.appendingPathComponent("YARD Drop/Inbox"))
      try JSONEncoder().encode(capture).write(to: captureURL, options: .atomic)
    }
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
      guard try readCapture(now: now) == capture else { throw DropError.invalidCapture }
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

  func publish(_ captureID: String, now: Int64 = DropStore.now) throws -> Int {
    try locked {
      let capture = try readCapture(now: now)
      guard capture.id == captureID else { throw DropError.invalidCapture }
      let source = incoming.appendingPathComponent(captureID)
      let batches =
        fm.fileExists(atPath: source.path)
        ? try fm.contentsOfDirectory(at: source, includingPropertiesForKeys: nil) : []
      let inbox = documents.appendingPathComponent("YARD Drop/Inbox/\(captureID)")
      try fm.createDirectory(at: inbox, withIntermediateDirectories: true)
      var count = 0
      for directory in batches {
        guard fm.fileExists(atPath: directory.appendingPathComponent(Self.marker).path) else {
          continue
        }
        let batch = try JSONDecoder().decode(
          DropBatch.self, from: Data(contentsOf: directory.appendingPathComponent("batch.json")))
        guard batch.schemaVersion == 1, batch.captureId == captureID,
          batch.batchId == directory.lastPathComponent, Self.validID(batch.batchId),
          !batch.files.isEmpty, batch.files.count <= 20
        else { throw DropError.invalidBatch }
        let destination = inbox.appendingPathComponent(batch.batchId)
        // A retry replaces an interrupted copy before writing the ready marker.
        try removeIfPresent(destination)
        try fm.createDirectory(at: destination, withIntermediateDirectories: true)
        var total: Int64 = 0
        var names: Set<String> = ["batch.json", Self.marker.lowercased()]
        for file in batch.files {
          guard file.name == Self.safeName(file.name),
            names.insert(file.name.lowercased()).inserted,
            file.stagedName == "\(file.id).bin", Self.validID(file.id), file.size >= 0
          else { throw DropError.invalidBatch }
          let size = try Self.copy(
            directory.appendingPathComponent(file.stagedName),
            to: destination.appendingPathComponent(file.name),
            remaining: Self.maxBatch - total
          )
          guard size == file.size else { throw DropError.invalidBatch }
          total += size
        }
        try JSONEncoder().encode(batch).write(
          to: destination.appendingPathComponent("batch.json"), options: .atomic)
        try Data().write(to: destination.appendingPathComponent(Self.marker), options: .atomic)
        count += batch.files.count
      }
      // Keep an empty capture armed so Finish transfer can be retried after staging completes.
      if count > 0 {
        try removeIfPresent(incoming)
        try removeIfPresent(captureURL)
      }
      return count
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
