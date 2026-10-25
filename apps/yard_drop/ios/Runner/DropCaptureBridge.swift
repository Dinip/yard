import Flutter
import Foundation

final class DropCaptureBridge {
  private let queue = DispatchQueue(label: "yard.drop.capture")
  private var status: [String: Any] = ["state": "idle"]
  private var channel: FlutterMethodChannel?

  func start(arguments: [String] = ProcessInfo.processInfo.arguments) {
    queue.async {
      do {
        let store = try DropStore.shared()
        guard let command = try DropCommand(arguments: arguments) else {
          _ = store.activeCapture()
          return
        }
        switch command {
        case .arm(let capture):
          try store.arm(capture)
          self.status = ["state": "armed"]
        case .publish(let captureID):
          let count = try store.publish(captureID)
          self.status = ["state": count == 0 ? "empty" : "published", "count": count]
        }
      } catch {
        self.status = ["state": "failed", "error": error.localizedDescription]
      }
    }
  }

  func register(messenger: FlutterBinaryMessenger) {
    let channel = FlutterMethodChannel(
      name: "com.dinispimpao.yard.drop/capture", binaryMessenger: messenger)
    self.channel = channel
    channel.setMethodCallHandler { [weak self] call, result in
      guard call.method == "captureStatus", let self else {
        result(FlutterMethodNotImplemented)
        return
      }
      self.queue.async {
        let status = self.status
        DispatchQueue.main.async { result(status) }
      }
    }
  }
}
