import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/share/incoming_share.dart';
import 'package:yard_drop/share/share_controller.dart';

import 'fake_share_gateway.dart';
import 'share_fixtures.dart';

void main() {
  late FakeShareGateway gateway;
  late ShareController controller;

  setUp(() {
    gateway = FakeShareGateway();
    controller = ShareController(gateway);
  });

  tearDown(() {
    controller.dispose();
    return gateway.dispose();
  });

  test('starts with nothing waiting', () async {
    await controller.start();

    expect(controller.current, isNull);
    expect(controller.waiting, 0);
  });

  test('picks up a share staged before Dart subscribed', () async {
    gateway.emit(shareFixture(state: IncomingShareState.receiving));

    await controller.start();

    expect(controller.current?.id, 's1');
    expect(controller.current?.state, IncomingShareState.receiving);
  });

  test('receiving becomes ready when the native side finishes', () async {
    await controller.start();
    gateway.emit(shareFixture(state: IncomingShareState.receiving));
    await pumpEventQueue();

    gateway.emit(shareFixture());
    await pumpEventQueue();

    expect(controller.current?.state, IncomingShareState.ready);
    expect(controller.waiting, 1);
  });

  test('a save goes ready, saving, saved', () async {
    await controller.start();
    gateway.emit(shareFixture());
    await pumpEventQueue();

    final seen = <IncomingShareState>[];
    controller.addListener(() => seen.add(controller.current!.state));

    final gate = Completer<SaveResult>();
    gateway.onSave = (_, _) => gate.future;
    final saving = controller.save(ShareDestination.downloads);
    await pumpEventQueue();
    expect(controller.current?.state, IncomingShareState.saving);

    gate.complete(const SaveResult.success(ShareDestination.downloads));
    await saving;

    expect(seen.first, IncomingShareState.saving);
    expect(seen.last, IncomingShareState.saved);
    expect(gateway.savedDestinations['s1'], ShareDestination.downloads);
  });

  test('a failed save keeps the error and can be retried', () async {
    await controller.start();
    gateway.emit(shareFixture());
    await pumpEventQueue();
    gateway.onSave = (_, destination) async =>
        SaveResult.failure(destination, 'no space left');

    await controller.save(ShareDestination.downloads);

    expect(controller.current?.state, IncomingShareState.failed);
    expect(controller.current?.error, 'no space left');

    controller.retry();

    expect(controller.current?.state, IncomingShareState.ready);
    expect(controller.current?.error, isNull);
  });

  test('a gateway that throws fails the share instead of the app', () async {
    await controller.start();
    gateway.emit(shareFixture());
    await pumpEventQueue();
    gateway.onSave = (_, _) async => throw StateError('channel is gone');

    await controller.save(ShareDestination.downloads);

    expect(controller.current?.state, IncomingShareState.failed);
    expect(controller.current?.error, contains('channel is gone'));
  });

  test('saving is refused for a share that is not ready', () async {
    await controller.start();
    gateway.emit(shareFixture(state: IncomingShareState.receiving));
    await pumpEventQueue();

    await controller.save(ShareDestination.downloads);

    expect(controller.current?.state, IncomingShareState.receiving);
    expect(gateway.savedDestinations, isEmpty);
  });

  test('a second share queues behind the first and is not lost', () async {
    await controller.start();
    gateway.emit(shareFixture(id: 's1'));
    gateway.emit(shareFixture(id: 's2'));
    await pumpEventQueue();

    expect(controller.waiting, 2);
    expect(controller.current?.id, 's1');

    await controller.dismiss();

    expect(controller.current?.id, 's2');
    expect(gateway.discarded, ['s1']);
  });

  test('a duplicate event updates the queued share in place', () async {
    gateway.emit(shareFixture(state: IncomingShareState.receiving));
    await controller.start();

    gateway.emit(shareFixture(state: IncomingShareState.receiving));
    await pumpEventQueue();

    expect(controller.waiting, 1);
  });

  test('a late ready event does not undo a save in flight', () async {
    await controller.start();
    gateway.emit(shareFixture());
    await pumpEventQueue();

    final gate = Completer<SaveResult>();
    gateway.onSave = (_, _) => gate.future;
    final saving = controller.save(ShareDestination.downloads);
    await pumpEventQueue();

    gateway.emit(shareFixture());
    await pumpEventQueue();
    expect(controller.current?.state, IncomingShareState.saving);

    gate.complete(const SaveResult.success(ShareDestination.downloads));
    await saving;
    expect(controller.current?.state, IncomingShareState.saved);
  });

  test('refresh reconciles a share missed while backgrounded', () async {
    await controller.start();
    await controller.dismiss();

    gateway.emit(shareFixture(id: 's9'));
    await controller.refresh();

    expect(controller.current?.id, 's9');
  });
}
