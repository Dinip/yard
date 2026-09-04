import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/share/incoming_share.dart';

import 'fake_share_gateway.dart';

void main() {
  late FakeShareGateway gateway;

  setUp(() => gateway = FakeShareGateway());
  tearDown(() => gateway.dispose());

  IncomingShare share(String id) => IncomingShare(
    id: id,
    receivedAt: DateTime.utc(2026),
    files: const [IncomingFile(id: 'f1', displayName: 'build.apk')],
    state: IncomingShareState.ready,
  );

  test('an emitted share is pending and reaches the change stream', () async {
    final seen = <String>[];
    gateway.changes.listen((s) => seen.add(s.id));

    gateway.emit(share('s1'));
    await pumpEventQueue();

    expect(await gateway.pendingShares(), hasLength(1));
    expect(seen, ['s1']);
  });

  test('a save records the destination and moves to saved', () async {
    gateway.emit(share('s1'));

    final result = await gateway.save('s1', ShareDestination.downloads);

    expect(result.succeeded, isTrue);
    expect(gateway.savedDestinations['s1'], ShareDestination.downloads);
    final pending = await gateway.pendingShares();
    expect(pending.single.state, IncomingShareState.saved);
  });

  test('an injected failure surfaces the error', () async {
    gateway.emit(share('s1'));
    gateway.onSave = (_, destination) =>
        SaveResult.failure(destination, 'no space left');

    final result = await gateway.save('s1', ShareDestination.browserInbox);

    expect(result.succeeded, isFalse);
    final pending = await gateway.pendingShares();
    expect(pending.single.error, 'no space left');
  });
}
