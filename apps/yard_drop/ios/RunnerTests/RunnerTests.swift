import UniformTypeIdentifiers
import XCTest

final class RunnerTests: XCTestCase {
  private var base: URL!
  private var store: DropStore!
  private let now: Int64 = 1_800_000_000_000
  private let captureID = UUID().uuidString

  override func setUpWithError() throws {
    base = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    store = DropStore(root: base.appendingPathComponent("Group"))
  }

  override func tearDownWithError() throws {
    if FileManager.default.fileExists(atPath: base.path) {
      try FileManager.default.removeItem(at: base)
    }
  }

  private func writeCapture(_ capture: DropCapture) throws {
    try FileManager.default.createDirectory(at: store.root, withIntermediateDirectories: true)
    try JSONEncoder().encode(capture).write(
      to: store.root.appendingPathComponent("active-capture.json"), options: .atomic)
  }

  private func arm() throws -> DropCapture {
    let capture = DropCapture(id: captureID, expiresAt: now + 60_000)
    try writeCapture(capture)
    return capture
  }

  private func staged() throws -> (DropBatch, URL) {
    let directory = try store.makeStagingDirectory()
    let id = UUID().uuidString
    let bytes = Data("company archive bytes".utf8)
    try bytes.write(to: directory.appendingPathComponent("report.zip"))
    return (
      DropBatch(
        schemaVersion: 1, batchId: UUID().uuidString, captureId: nil, createdAt: now,
        producer: .init(appVersion: "0.6.5", buildNumber: 1, commit: "test"),
        files: [
          .init(
            id: id, name: "report.zip", typeIdentifier: "public.zip-archive",
            mimeType: "application/zip", size: Int64(bytes.count), stagedName: "report.zip")
        ]), directory
    )
  }

  func testCaptureMustBeFreshAndShortLived() throws {
    for expiry in [now, now - 1, now + DropStore.captureLifetime + 1] {
      try writeCapture(.init(id: captureID, expiresAt: expiry))
      XCTAssertNil(store.activeCapture(now: now))
    }
    try writeCapture(.init(id: "../invalid", expiresAt: now + 60_000))
    XCTAssertNil(store.activeCapture(now: now))
    _ = try arm()
    XCTAssertNotNil(store.activeCapture(now: now))
  }

  func testNewReservationRejectsAnInFlightShare() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    try writeCapture(.init(id: UUID().uuidString, expiresAt: now + 60_000))
    XCTAssertThrowsError(try store.queue(batch, staged: directory, capture: capture, now: now))
    XCTAssertTrue(FileManager.default.fileExists(atPath: directory.path))
  }

  func testLeaseRefreshKeepsAnInFlightShareAndPreservesBytes() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    let expected = try Data(contentsOf: directory.appendingPathComponent("report.zip"))
    try writeCapture(.init(id: capture.id, expiresAt: now + 120_000))
    try store.queue(batch, staged: directory, capture: capture, now: now)
    let queued = store.root.appendingPathComponent("Incoming/\(captureID)/\(batch.batchId)")
    XCTAssertEqual(try Data(contentsOf: queued.appendingPathComponent("report.zip")), expected)
    XCTAssertTrue(
      FileManager.default.fileExists(atPath: queued.appendingPathComponent(DropStore.marker).path))
    let manifest = try JSONDecoder().decode(
      DropBatch.self, from: Data(contentsOf: queued.appendingPathComponent("batch.json")))
    XCTAssertEqual(manifest.captureId, capture.id)
    XCTAssertEqual(manifest.files.first?.name, "report.zip")
    XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path))
    XCTAssertNotNil(store.activeCapture(now: now))
  }

  func testExpiredLeaseRejectsNewSharesWithoutDeletingCompletedBatches() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    try store.queue(batch, staged: directory, capture: capture, now: now)
    XCTAssertNil(store.activeCapture(now: now + 60_000))
    let queued = store.root.appendingPathComponent(
      "Incoming/\(captureID)/\(batch.batchId)/report.zip")
    XCTAssertTrue(FileManager.default.fileExists(atPath: queued.path))
    let (next, staging) = try staged()
    XCTAssertThrowsError(
      try store.queue(next, staged: staging, capture: capture, now: now + 60_000))
  }

  func testNamesCannotEscapeAndDuplicatesKeepExtensions() {
    XCTAssertEqual(DropStore.safeName("../a:b.zip"), ".._a_b.zip")
    XCTAssertEqual(DropStore.safeName(".."), "attachment")
    XCTAssertLessThanOrEqual(DropStore.safeName(String(repeating: "🦊", count: 180)).utf8.count, 180)
    var used: Set<String> = ["batch.json", "_yard_ready"]
    XCTAssertEqual(DropStore.uniqueName("batch.json", used: &used), "batch (1).json")
    XCTAssertEqual(DropStore.uniqueName("A.ZIP", used: &used), "A.ZIP")
    XCTAssertEqual(DropStore.uniqueName("a.zip", used: &used), "a (1).zip")
  }

  func testCopyCountsActualBytesAndRemovesOverLimitAndCancelledFiles() throws {
    let directory = try store.makeStagingDirectory()
    let input = directory.appendingPathComponent("input")
    let output = directory.appendingPathComponent("output")
    let data = Data(repeating: 0x7f, count: 1_000_000)
    try data.write(to: input)
    XCTAssertEqual(
      try DropStore.copy(input, to: output, remaining: DropStore.maxBatch), Int64(data.count))
    XCTAssertEqual(try Data(contentsOf: output), data)
    XCTAssertThrowsError(try DropStore.copy(input, to: output, remaining: 20))
    XCTAssertFalse(FileManager.default.fileExists(atPath: output.path))
    XCTAssertThrowsError(
      try DropStore.copy(input, to: output, remaining: DropStore.maxBatch, cancelled: { true }))
    XCTAssertFalse(FileManager.default.fileExists(atPath: output.path))
  }

  @MainActor func testItemProvidersCopyInsideCallbackAndKeepSuccessfulAttachments() async throws {
    let directory = try store.makeStagingDirectory()
    let input = directory.appendingPathComponent("source.zip")
    try Data("zip bytes".utf8).write(to: input)
    let provider = NSItemProvider()
    provider.suggestedName = "report.zip"
    provider.registerFileRepresentation(
      forTypeIdentifier: UTType.zip.identifier, fileOptions: [], visibility: .all
    ) { completion in
      completion(input, false, nil)
      return nil
    }
    let failed = NSItemProvider()
    failed.registerFileRepresentation(
      forTypeIdentifier: UTType.pdf.identifier, fileOptions: [], visibility: .all
    ) { completion in
      completion(nil, false, DropError.invalidBatch)
      return nil
    }
    let model = ShareModel()
    model.receive([provider, provider, failed], store: store)
    for _ in 0..<200 where model.busy { try await Task.sleep(nanoseconds: 10_000_000) }
    XCTAssertFalse(model.busy)
    XCTAssertEqual(model.files.count, 1)
    XCTAssertEqual(model.files.first?.size, 9)
    XCTAssertEqual(model.failures.count, 1)
    model.discard()
  }
}
