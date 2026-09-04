enum IncomingShareState { receiving, ready, saving, saved, failed }

enum ShareDestination { downloads, browserInbox }

/// Where one attachment in a batch got to.
enum IncomingFileState { pending, saved, failed }

final class IncomingFile {
  const IncomingFile({
    required this.id,
    required this.displayName,
    this.mimeType,
    this.reportedSize,
    this.state = IncomingFileState.pending,
    this.error,
    this.savedName,
  });

  final String id;
  final String displayName;
  final String? mimeType;
  final int? reportedSize;
  final IncomingFileState state;
  final String? error;

  /// The name Downloads gave it, which a duplicate makes differ.
  final String? savedName;
}

final class IncomingShare {
  const IncomingShare({
    required this.id,
    required this.receivedAt,
    required this.files,
    required this.state,
    this.error,
    this.progress,
    this.savedLocation,
  });

  final String id;
  final DateTime receivedAt;
  final List<IncomingFile> files;
  final IncomingShareState state;
  final String? error;

  /// 0..1 while saving, when the native side knows the total size.
  final double? progress;

  /// Where the files ended up, once they have.
  final String? savedLocation;

  bool get isBatch => files.length > 1;

  Iterable<IncomingFile> get pendingFiles =>
      files.where((file) => file.state == IncomingFileState.pending);

  Iterable<IncomingFile> get savedFiles =>
      files.where((file) => file.state == IncomingFileState.saved);

  Iterable<IncomingFile> get failedFiles =>
      files.where((file) => file.state == IncomingFileState.failed);

  // A retry has to be able to clear the previous failure, which `error: null`
  // alone cannot express.
  IncomingShare copyWith({
    IncomingShareState? state,
    String? error,
    bool clearError = false,
    double? progress,
    String? savedLocation,
  }) {
    return IncomingShare(
      id: id,
      receivedAt: receivedAt,
      files: files,
      state: state ?? this.state,
      error: clearError ? null : error ?? this.error,
      progress: progress ?? this.progress,
      savedLocation: savedLocation ?? this.savedLocation,
    );
  }
}

final class SaveResult {
  const SaveResult.success(this.destination, {this.location})
    : succeeded = true,
      error = null;

  const SaveResult.failure(this.destination, this.error)
    : succeeded = false,
      location = null;

  final ShareDestination destination;
  final bool succeeded;
  final String? error;

  /// The folder the files landed in, for the screen to name.
  final String? location;
}
