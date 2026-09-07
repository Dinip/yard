import 'dart:async';

import 'package:flutter/services.dart';

import '../build_info.dart';
import 'incoming_share.dart';
import 'share_gateway.dart';

const _methodChannel = MethodChannel('com.dinispimpao.yard.drop/share');
const _eventChannel = EventChannel('com.dinispimpao.yard.drop/share_events');

/// Talks to the Android share receiver. Only metadata crosses: the native side
/// holds the attachments and does every read and write itself.
final class PlatformShareGateway implements ShareGateway {
  @override
  Stream<IncomingShare> get changes => _eventChannel
      .receiveBroadcastStream()
      .map((event) => _decodeShare(event as Map<Object?, Object?>));

  @override
  Future<List<IncomingShare>> pendingShares() async {
    final shares = await _methodChannel.invokeListMethod<Object?>(
      'listPendingShares',
    );
    return (shares ?? const [])
        .map((share) => _decodeShare(share! as Map<Object?, Object?>))
        .toList();
  }

  @override
  Future<SaveResult> save(String shareId, ShareDestination destination) async {
    try {
      const build = BuildInfo.current;
      final location = await _methodChannel.invokeMethod<String>('saveShare', {
        'shareId': shareId,
        'destination': destination.name,
        // A browser batch records the build that wrote it, so an unsupported
        // schema can name the version the device is running.
        'appVersion': build.version,
        'buildNumber': build.buildNumber,
        'commit': build.commit,
      });
      return SaveResult.success(destination, location: location);
    } on PlatformException catch (error) {
      return SaveResult.failure(destination, error.message ?? error.code);
    }
  }

  @override
  Future<void> discard(String shareId) {
    return _methodChannel.invokeMethod<void>('discardShare', {
      'shareId': shareId,
    });
  }
}

IncomingShare _decodeShare(Map<Object?, Object?> wire) {
  final files = (wire['files'] as List<Object?>? ?? const [])
      .map((file) => _decodeFile(file! as Map<Object?, Object?>))
      .toList();

  return IncomingShare(
    id: wire['id']! as String,
    receivedAt: DateTime.fromMillisecondsSinceEpoch(wire['receivedAt']! as int),
    files: files,
    state: _decodeState(wire['state']! as String),
    error: wire['error'] as String?,
    progress: (wire['progress'] as num?)?.toDouble(),
  );
}

IncomingFile _decodeFile(Map<Object?, Object?> wire) {
  return IncomingFile(
    id: wire['id']! as String,
    displayName: wire['displayName']! as String,
    mimeType: wire['mimeType'] as String?,
    reportedSize: (wire['reportedSize'] as num?)?.toInt(),
    state: IncomingFileState.values.firstWhere(
      (state) => state.name == wire['state'],
      orElse: () => IncomingFileState.pending,
    ),
    error: wire['error'] as String?,
    savedName: wire['savedName'] as String?,
  );
}

IncomingShareState _decodeState(String wire) {
  return IncomingShareState.values.firstWhere(
    (state) => state.name == wire,
    // A newer native side must not crash an older screen mid-transfer.
    orElse: () => IncomingShareState.receiving,
  );
}
