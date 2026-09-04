import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/share/incoming_share.dart';
import 'package:yard_drop/share/platform_share_gateway.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const channel = MethodChannel('com.dinispimpao.yard.drop/share');
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  final calls = <MethodCall>[];
  late PlatformShareGateway gateway;

  void answer(Future<Object?> Function(MethodCall call) handler) {
    messenger.setMockMethodCallHandler(channel, (call) {
      calls.add(call);
      return handler(call);
    });
  }

  setUp(() {
    calls.clear();
    gateway = PlatformShareGateway();
  });

  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  test('decodes a pending share, including absent metadata', () async {
    answer(
      (_) async => [
        {
          'id': 's1',
          'receivedAt': 1774000000000,
          'state': 'ready',
          'error': null,
          'files': [
            {
              'id': 'f1',
              'displayName': 'build.apk',
              'mimeType': 'application/vnd.android.package-archive',
              'reportedSize': 4200000,
            },
            {
              'id': 'f2',
              'displayName': 'shared-1774000000000',
              'mimeType': null,
              'reportedSize': null,
            },
          ],
        },
      ],
    );

    final shares = await gateway.pendingShares();

    expect(shares, hasLength(1));
    final share = shares.single;
    expect(share.id, 's1');
    expect(share.state, IncomingShareState.ready);
    expect(
      share.receivedAt,
      DateTime.fromMillisecondsSinceEpoch(1774000000000),
    );
    expect(share.files.first.reportedSize, 4200000);
    expect(share.files.last.mimeType, isNull);
    expect(share.files.last.reportedSize, isNull);
  });

  test('per-file state and saved names decode', () async {
    answer(
      (_) async => [
        {
          'id': 's1',
          'receivedAt': 1774000000000,
          'state': 'failed',
          'error': '1 of 2 files are in Download/YARD Drop/Saved.',
          'files': [
            {
              'id': 'f1',
              'displayName': 'one.png',
              'state': 'saved',
              'savedName': 'one (1).png',
            },
            {
              'id': 'f2',
              'displayName': 'two.png',
              'state': 'failed',
              'error': 'It could not be written to the YARD Drop folder.',
            },
          ],
        },
      ],
    );

    final share = (await gateway.pendingShares()).single;

    expect(share.savedFiles.single.savedName, 'one (1).png');
    expect(share.failedFiles.single.displayName, 'two.png');
    expect(share.pendingFiles, isEmpty);
  });

  test('a rejected text share arrives as a failure with no files', () async {
    answer(
      (_) async => [
        {
          'id': 's1',
          'receivedAt': 1774000000000,
          'state': 'failed',
          'error': 'YARD Drop needs a file attachment.',
          'files': <Object?>[],
        },
      ],
    );

    final share = (await gateway.pendingShares()).single;

    expect(share.state, IncomingShareState.failed);
    expect(share.files, isEmpty);
    expect(share.error, 'YARD Drop needs a file attachment.');
  });

  test('an unknown state does not crash an older screen', () async {
    answer(
      (_) async => [
        {
          'id': 's1',
          'receivedAt': 1774000000000,
          'state': 'transcoding',
          'files': <Object?>[],
        },
      ],
    );

    final share = (await gateway.pendingShares()).single;

    expect(share.state, IncomingShareState.receiving);
  });

  test('no pending shares decodes as an empty list', () async {
    answer((_) async => null);

    expect(await gateway.pendingShares(), isEmpty);
  });

  test('save passes the destination and reports success', () async {
    answer((_) async => null);

    final result = await gateway.save('s1', ShareDestination.downloads);

    expect(result.succeeded, isTrue);
    expect(calls.single.method, 'saveShare');
    expect(calls.single.arguments, {
      'shareId': 's1',
      'destination': 'downloads',
    });
  });

  test('a platform error becomes a failed save, not an exception', () async {
    answer(
      (_) async => throw PlatformException(
        code: 'no_space',
        message: 'no space left on device',
      ),
    );

    final result = await gateway.save('s1', ShareDestination.downloads);

    expect(result.succeeded, isFalse);
    expect(result.error, 'no space left on device');
  });

  test('discard names the share', () async {
    answer((_) async => null);

    await gateway.discard('s1');

    expect(calls.single.method, 'discardShare');
    expect(calls.single.arguments, {'shareId': 's1'});
  });
}
