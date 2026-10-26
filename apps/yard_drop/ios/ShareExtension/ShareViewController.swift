import SwiftUI
import UIKit
import UniformTypeIdentifiers

final class ShareViewController: UIViewController, UIDocumentPickerDelegate {
  private let model = ShareModel()

  override func viewDidLoad() {
    super.viewDidLoad()
    let host = UIHostingController(
      rootView: ShareView(
        model: model,
        save: { [weak self] in self?.save() },
        send: { [weak self] in self?.send() },
        cancel: { [weak self] in self?.cancel() }
      ))
    addChild(host)
    host.view.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(host.view)
    NSLayoutConstraint.activate([
      host.view.topAnchor.constraint(equalTo: view.topAnchor),
      host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
      host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
    ])
    host.didMove(toParent: self)
    let providers = (extensionContext?.inputItems as? [NSExtensionItem] ?? []).flatMap {
      $0.attachments ?? []
    }
    model.receive(providers)
  }

  private func send() {
    model.send { [weak self] in
      self?.extensionContext?.completeRequest(returningItems: nil)
    }
  }

  private func save() {
    model.export { [weak self] urls in
      guard let self else { return }
      let picker = UIDocumentPickerViewController(forExporting: urls, asCopy: true)
      picker.delegate = self
      self.present(picker, animated: true)
    }
  }

  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL])
  {
    model.discard()
    extensionContext?.completeRequest(returningItems: nil)
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    model.busy = false
  }

  private func cancel() {
    model.discard()
    extensionContext?.cancelRequest(withError: DropError.cancelled)
  }
}

final class ShareModel: ObservableObject {
  @Published var files: [DropFile] = []
  @Published var failures: [String] = []
  @Published var busy = true
  @Published var capture: DropCapture?
  private var store: DropStore?
  private var directory: URL?
  private var batch: DropBatch?
  private var task: Task<Void, Never>?
  private var progress: Progress?
  private let cancellation = Progress(totalUnitCount: 1)

  static func typeIdentifier(_ provider: NSItemProvider) -> String? {
    provider.registeredTypeIdentifiers.first {
      guard let type = UTType($0) else { return false }
      return type.conforms(to: .data) && !type.conforms(to: .url)
    }
  }

  func receive(_ providers: [NSItemProvider], store suppliedStore: DropStore? = nil) {
    task = Task { @MainActor in
      defer { busy = false }
      do {
        guard !providers.isEmpty, providers.count <= 20 else { throw DropError.limit }
        let store = try suppliedStore ?? DropStore.shared()
        self.store = store
        capture = store.activeCapture()
        let directory = try store.makeStagingDirectory()
        self.directory = directory
        var total: Int64 = 0
        var used: Set<String> = ["batch.json", DropStore.marker.lowercased()]
        // Each provider contributes one representation, even when it advertises several types.
        var seen = Set<ObjectIdentifier>()
        for provider in providers where seen.insert(ObjectIdentifier(provider)).inserted {
          try Task.checkCancellation()
          do {
            guard let type = Self.typeIdentifier(provider) else { throw DropError.invalidBatch }
            let id = UUID().uuidString
            let destination = directory.appendingPathComponent("\(id).bin")
            let remaining = DropStore.maxBatch - total
            let cancellation = self.cancellation
            let (size, sourceName): (Int64, String) = try await withCheckedThrowingContinuation {
              continuation in
              progress = provider.loadFileRepresentation(forTypeIdentifier: type) { url, error in
                do {
                  guard let url else { throw error ?? DropError.invalidBatch }
                  // This URL expires when the callback returns, so finish the bounded copy here.
                  let size = try DropStore.copy(
                    url, to: destination, remaining: remaining,
                    cancelled: { cancellation.isCancelled })
                  continuation.resume(returning: (size, url.lastPathComponent))
                } catch { continuation.resume(throwing: error) }
              }
            }
            try Task.checkCancellation()
            var suggested = provider.suggestedName ?? sourceName
            if (suggested as NSString).pathExtension.isEmpty,
              let ext = UTType(type)?.preferredFilenameExtension
            {
              suggested += ".\(ext)"
            }
            let name = DropStore.uniqueName(suggested, used: &used)
            try FileManager.default.moveItem(
              at: destination, to: directory.appendingPathComponent(name))
            let file = DropFile(
              id: id, name: name, typeIdentifier: type,
              mimeType: UTType(type)?.preferredMIMEType ?? "application/octet-stream", size: size,
              stagedName: name
            )
            total += size
            files.append(file)
          } catch is CancellationError { throw DropError.cancelled } catch {
            failures.append(
              "Attachment \(files.count + failures.count + 1): \(error.localizedDescription)")
          }
        }
        batch = DropBatch(
          schemaVersion: 1, batchId: UUID().uuidString, captureId: nil,
          createdAt: DropStore.now, producer: .current, files: files
        )
      } catch { failures.append(error.localizedDescription) }
      if Task.isCancelled { cleanup() }
    }
  }

  func send(completion: @escaping () -> Void) {
    guard !busy, let store, let directory, let batch, let capture, !files.isEmpty else { return }
    busy = true
    do {
      try store.queue(batch, staged: directory, capture: capture)
      self.directory = nil
      completion()
    } catch {
      failures.append(error.localizedDescription)
      busy = false
    }
  }

  func export(completion: @escaping ([URL]) -> Void) {
    guard !busy, let directory, !files.isEmpty else { return }
    busy = true
    completion(files.map { directory.appendingPathComponent($0.stagedName) })
  }

  func discard() {
    task?.cancel()
    cancellation.cancel()
    progress?.cancel()
    // A provider callback can still own the staging URL; its task cleans up after it returns.
    if !busy { cleanup() }
  }

  private func cleanup() {
    if let directory { try? FileManager.default.removeItem(at: directory) }
    directory = nil
  }
}

private struct ShareView: View {
  @ObservedObject var model: ShareModel
  let save: () -> Void
  let send: () -> Void
  let cancel: () -> Void

  var body: some View {
    NavigationView {
      List {
        if model.busy { ProgressView("Copying files…") }
        ForEach(model.files, id: \.id) { file in
          VStack(alignment: .leading) {
            Text(file.name)
            Text(ByteCountFormatter.string(fromByteCount: file.size, countStyle: .file)).font(
              .caption)
          }
        }
        ForEach(Array(model.failures.enumerated()), id: \.offset) { _, message in
          Text(message).foregroundColor(.red)
        }
        Section {
          Button("Save on this device", action: save).disabled(model.busy || model.files.isEmpty)
          Button("Send to YARD browser", action: send)
            .disabled(model.busy || model.files.isEmpty || model.capture == nil)
          Text(
            model.capture == nil
              ? "Reserve this iPhone in YARD to send files to the browser."
              : "Files appear automatically in Receive shared files in your YARD browser."
          )
          .font(.caption)
        }
      }
      .navigationTitle("YARD - Device Farm")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) } }
    }
    .navigationViewStyle(.stack)
  }
}
