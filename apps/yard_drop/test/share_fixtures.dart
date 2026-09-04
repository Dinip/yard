import 'package:yard_drop/share/incoming_share.dart';

IncomingShare shareFixture({
  String id = 's1',
  IncomingShareState state = IncomingShareState.ready,
  List<IncomingFile>? files,
  String? error,
  DateTime? receivedAt,
}) {
  return IncomingShare(
    id: id,
    receivedAt: receivedAt ?? DateTime.utc(2026),
    files:
        files ??
        const [
          IncomingFile(
            id: 'f1',
            displayName: 'build.apk',
            mimeType: 'application/vnd.android.package-archive',
            reportedSize: 4200000,
          ),
        ],
    state: state,
    error: error,
  );
}
