enum IncomingShareState { receiving, ready, saving, saved, failed }

enum ShareDestination { downloads, browserInbox }

final class IncomingFile {
  const IncomingFile({
    required this.id,
    required this.displayName,
    this.mimeType,
    this.reportedSize,
  });

  final String id;
  final String displayName;
  final String? mimeType;
  final int? reportedSize;
}

final class IncomingShare {
  const IncomingShare({
    required this.id,
    required this.receivedAt,
    required this.files,
    required this.state,
    this.error,
  });

  final String id;
  final DateTime receivedAt;
  final List<IncomingFile> files;
  final IncomingShareState state;
  final String? error;

  bool get isBatch => files.length > 1;

  // A retry has to be able to clear the previous failure, which `error: null`
  // alone cannot express.
  IncomingShare copyWith({
    IncomingShareState? state,
    String? error,
    bool clearError = false,
  }) {
    return IncomingShare(
      id: id,
      receivedAt: receivedAt,
      files: files,
      state: state ?? this.state,
      error: clearError ? null : error ?? this.error,
    );
  }
}

final class SaveResult {
  const SaveResult.success(this.destination) : succeeded = true, error = null;

  const SaveResult.failure(this.destination, this.error) : succeeded = false;

  final ShareDestination destination;
  final bool succeeded;
  final String? error;
}
