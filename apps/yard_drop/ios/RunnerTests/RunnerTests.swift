import UniformTypeIdentifiers
import XCTest

final class RunnerTests: XCTestCase {
  private var base: URL!
  private var store: DropStore!
  private let now: Int64 = 1_800_000_000_000
  private let captureID = UUID().uuidString

  override func setUpWithError() throws {
    base = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    store = DropStore(
      root: base.appendingPathComponent("Group"),
      documents: base.appendingPathComponent("Documents"))
  }

  override func tearDownWithError() throws {
    if FileManager.default.fileExists(atPath: base.path) {
      try FileManager.default.removeItem(at: base)
    }
  }

  private func arm() throws -> DropCapture {
    let capture = DropCapture(id: captureID, expiresAt: now + 60_000)
    try store.arm(capture, now: now)
    return capture
  }

  private func staged() throws -> (DropBatch, URL) {
    let directory = try store.makeStagingDirectory()
    let id = UUID().uuidString
    let bytes = Data("company archive bytes".utf8)
    try bytes.write(to: directory.appendingPathComponent("\(id).bin"))
    return (
      DropBatch(
        schemaVersion: 1, batchId: UUID().uuidString, captureId: nil, createdAt: now,
        producer: .init(appVersion: "0.6.5", buildNumber: 1, commit: "test"),
        files: [
          .init(
            id: id, name: "report.zip", typeIdentifier: "public.zip-archive",
            mimeType: "application/zip", size: Int64(bytes.count), stagedName: "\(id).bin")
        ]), directory
    )
  }

  func testOrdinaryLaunchDoesNothingAndCommandsValidate() throws {
    XCTAssertNil(try DropCommand(arguments: ["Runner"]))
    XCTAssertNotNil(
      try DropCommand(arguments: ["Runner", "--yard-drop", "arm", captureID, String(now + 60_000)]))
    for args in [
      ["arm", "../bad", "1"], ["arm", captureID], ["publish", captureID, "extra"],
      ["unknown", captureID],
    ] {
      XCTAssertThrowsError(try DropCommand(arguments: ["Runner", "--yard-drop"] + args))
    }
  }

  func testCaptureMustBeFreshAndShortLived() throws {
    for expiry in [now, now - 1, now + DropStore.captureLifetime + 1] {
      XCTAssertThrowsError(try store.arm(.init(id: captureID, expiresAt: expiry), now: now))
    }
    _ = try arm()
    XCTAssertNil(store.activeCapture(now: now + 60_000))
  }

  func testNewCapturePurgesOldBatchesAndPublishedFiles() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    try store.queue(batch, staged: directory, capture: capture, now: now)
    let old = store.documents.appendingPathComponent("YARD Drop/Inbox/old")
    try FileManager.default.createDirectory(at: old, withIntermediateDirectories: true)
    try store.arm(.init(id: UUID().uuidString, expiresAt: now + 60_000), now: now)
    XCTAssertFalse(FileManager.default.fileExists(atPath: old.path))
    XCTAssertThrowsError(try store.publish(captureID, now: now))
    XCTAssertThrowsError(
      try store.queue(batch, staged: try store.makeStagingDirectory(), capture: capture, now: now))
  }

  func testPublishRequiresMatchingUnexpiredCaptureAndPreservesBytes() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    let expected = try Data(contentsOf: directory.appendingPathComponent(batch.files[0].stagedName))
    try store.queue(batch, staged: directory, capture: capture, now: now)
    XCTAssertThrowsError(try store.publish(UUID().uuidString, now: now))
    XCTAssertThrowsError(try store.publish(captureID, now: now + 60_000))
    XCTAssertEqual(try store.publish(captureID, now: now), 1)
    let published = store.documents.appendingPathComponent(
      "YARD Drop/Inbox/\(captureID)/\(batch.batchId)")
    XCTAssertEqual(try Data(contentsOf: published.appendingPathComponent("report.zip")), expected)
    XCTAssertTrue(
      FileManager.default.fileExists(
        atPath: published.appendingPathComponent(DropStore.marker).path))
    XCTAssertNil(store.activeCapture(now: now))
  }

  func testEmptyPublishCanBeRetriedAndIncompleteBatchIsIgnored() throws {
    _ = try arm()
    let partial = store.root.appendingPathComponent("Incoming/\(captureID)/\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: partial, withIntermediateDirectories: true)
    XCTAssertEqual(try store.publish(captureID, now: now), 0)
    XCTAssertNotNil(store.activeCapture(now: now))
  }

  func testFailedPublishNeverLooksReady() throws {
    let capture = try arm()
    let (batch, directory) = try staged()
    try store.queue(batch, staged: directory, capture: capture, now: now)
    let source = store.root.appendingPathComponent(
      "Incoming/\(captureID)/\(batch.batchId)/\(batch.files[0].stagedName)")
    try FileManager.default.removeItem(at: source)
    XCTAssertThrowsError(try store.publish(captureID, now: now))
    XCTAssertFalse(
      FileManager.default.fileExists(
        atPath: store.documents.appendingPathComponent(
          "YARD Drop/Inbox/\(captureID)/\(batch.batchId)/\(DropStore.marker)"
        ).path))
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
