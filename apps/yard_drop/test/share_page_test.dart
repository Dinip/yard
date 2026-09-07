import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/app.dart';
import 'package:yard_drop/share/incoming_share.dart';

import 'fake_share_gateway.dart';
import 'share_fixtures.dart';

void main() {
  late FakeShareGateway gateway;

  setUp(() => gateway = FakeShareGateway());
  tearDown(() => gateway.dispose());

  Future<void> pumpApp(WidgetTester tester) async {
    // A handset's height, so a batch of files and both destination buttons fit
    // on screen the way they do on a device.
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(YardDropApp(gateway: gateway));
    await tester.pump();
  }

  testWidgets('empty explains how a share reaches the app', (tester) async {
    await pumpApp(tester);

    expect(find.text('Nothing to drop yet'), findsOneWidget);
    expect(find.textContaining('Share a file from any app'), findsOneWidget);
  });

  testWidgets('receiving shows progress and no destination', (tester) async {
    gateway.emit(shareFixture(state: IncomingShareState.receiving));
    await pumpApp(tester);

    expect(find.text('Receiving'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    expect(find.text('Save on this device'), findsNothing);
  });

  testWidgets('a share still arriving can be cancelled', (tester) async {
    final share = shareFixture(state: IncomingShareState.receiving);
    gateway.emit(share);
    await pumpApp(tester);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();

    expect(gateway.discarded, [share.id]);
    expect(find.text('Nothing to drop yet'), findsOneWidget);
  });

  testWidgets('one ready file shows its name, size and type', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    expect(find.text('File ready'), findsOneWidget);
    expect(find.text('build.apk'), findsOneWidget);
    expect(find.textContaining('4.2 MB'), findsOneWidget);
    expect(find.text('Save on this device'), findsOneWidget);
  });

  testWidgets('a batch lists every file', (tester) async {
    gateway.emit(
      shareFixture(
        files: const [
          IncomingFile(id: 'f1', displayName: 'one.png'),
          IncomingFile(id: 'f2', displayName: 'two.png'),
          IncomingFile(id: 'f3', displayName: 'three.png'),
        ],
      ),
    );
    await pumpApp(tester);

    expect(find.text('3 files ready'), findsOneWidget);
    expect(find.text('two.png'), findsOneWidget);
    // Metadata the sharing app did not provide must not read as a real value.
    expect(find.text('unknown type'), findsNWidgets(3));
  });

  testWidgets('a queued second share is announced', (tester) async {
    gateway.emit(shareFixture(id: 's1'));
    gateway.emit(shareFixture(id: 's2'));
    await pumpApp(tester);

    expect(find.text('1 more share waiting'), findsOneWidget);
  });

  testWidgets('the browser inbox is the second destination', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    await tester.tap(find.text('Send to YARD browser'));
    await tester.pumpAndSettle();

    expect(gateway.savedDestinations['s1'], ShareDestination.browserInbox);
    expect(find.text('Ready for the browser'), findsOneWidget);
    // The batch folder ends in an id nobody needs, so it is not read out.
    expect(find.textContaining('Download/YARD Drop/Inbox/'), findsNothing);
  });

  testWidgets('saving shows progress until it completes', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    final gate = Completer<SaveResult>();
    gateway.onSave = (_, _) => gate.future;
    await tester.tap(find.text('Save on this device'));
    await tester.pump();

    expect(find.text('Saving'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);

    gate.complete(
      const SaveResult.success(
        ShareDestination.downloads,
        location: 'Download/YARD Drop/Saved',
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('Saved'), findsOneWidget);
    expect(
      find.textContaining('build.apk is in Download/YARD Drop/Saved'),
      findsOneWidget,
    );
  });

  testWidgets('a batch save shows how far it has got', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    final gate = Completer<SaveResult>();
    gateway.onSave = (_, _) => gate.future;
    await tester.tap(find.text('Save on this device'));
    await tester.pump();

    gateway.emit(shareFixture(state: IncomingShareState.saving, progress: 0.5));
    await tester.pump();

    final indicator = tester.widget<CircularProgressIndicator>(
      find.byType(CircularProgressIndicator),
    );
    expect(indicator.value, 0.5);

    gate.complete(const SaveResult.success(ShareDestination.downloads));
    await tester.pumpAndSettle();
    expect(find.text('Saved'), findsOneWidget);
  });

  testWidgets('done clears the share and returns to empty', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    await tester.tap(find.text('Save on this device'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();

    expect(find.text('Nothing to drop yet'), findsOneWidget);
    expect(gateway.discarded, ['s1']);
  });

  testWidgets('a failure shows the reason and offers a retry', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);
    gateway.onSave = (_, destination) async =>
        SaveResult.failure(destination, 'no space left on device');

    await tester.tap(find.text('Save on this device'));
    await tester.pumpAndSettle();

    expect(find.text('Not saved'), findsOneWidget);
    expect(find.text('no space left on device'), findsOneWidget);

    gateway.onSave = null;
    await tester.tap(find.text('Try again'));
    await tester.pumpAndSettle();
    expect(find.text('File ready'), findsOneWidget);

    await tester.tap(find.text('Save on this device'));
    await tester.pumpAndSettle();
    expect(find.text('Saved'), findsOneWidget);
  });

  testWidgets('a renamed duplicate is named on the saved screen', (
    tester,
  ) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);
    gateway.onSave = (_, destination) async =>
        SaveResult.success(destination, location: 'Download/YARD Drop/Saved');

    await tester.tap(find.text('Save on this device'));
    await tester.pumpAndSettle();
    gateway.emit(
      shareFixture(
        state: IncomingShareState.saved,
        files: const [
          IncomingFile(
            id: 'f1',
            displayName: 'build.apk',
            state: IncomingFileState.saved,
            savedName: 'build (1).apk',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('Saved as build (1).apk'), findsOneWidget);
  });

  testWidgets('a batch shows what saved and what did not', (tester) async {
    gateway.emit(
      shareFixture(
        state: IncomingShareState.failed,
        error: '2 of 3 files are in Download/YARD Drop/Saved.',
        files: const [
          IncomingFile(
            id: 'f1',
            displayName: 'one.png',
            state: IncomingFileState.saved,
          ),
          IncomingFile(
            id: 'f2',
            displayName: 'two.png',
            state: IncomingFileState.saved,
            savedName: 'two (1).png',
          ),
          IncomingFile(
            id: 'f3',
            displayName: 'three.png',
            state: IncomingFileState.failed,
            error: 'It could not be written to the YARD Drop folder.',
          ),
        ],
      ),
    );
    await pumpApp(tester);

    expect(find.text('one.png'), findsOneWidget);
    expect(find.text('Saved'), findsOneWidget);
    expect(find.text('Saved as two (1).png'), findsOneWidget);
    expect(
      find.text('It could not be written to the YARD Drop folder.'),
      findsOneWidget,
    );
  });

  testWidgets('a batch counts only what is still to save', (tester) async {
    gateway.emit(
      shareFixture(
        files: const [
          IncomingFile(id: 'f1', displayName: 'one.png'),
          IncomingFile(id: 'f2', displayName: 'two.png'),
          IncomingFile(
            id: 'f3',
            displayName: 'three.png',
            state: IncomingFileState.failed,
            error: 'The file could not be read from the app that shared it.',
          ),
        ],
      ),
    );
    await pumpApp(tester);

    expect(find.text('2 files ready'), findsOneWidget);
    expect(find.text('three.png'), findsOneWidget);
    expect(find.text('Save on this device'), findsOneWidget);
  });

  testWidgets('a share that never arrived cannot be retried', (tester) async {
    gateway.emit(
      shareFixture(
        state: IncomingShareState.failed,
        files: const [],
        error: 'YARD - Device Farm needs a file attachment.',
      ),
    );
    await pumpApp(tester);

    expect(find.text('Share not received'), findsOneWidget);
    expect(
      find.text('YARD - Device Farm needs a file attachment.'),
      findsOneWidget,
    );
    expect(find.text('Try again'), findsNothing);
    expect(find.text('Discard'), findsOneWidget);
  });

  testWidgets('about shows the build identity', (tester) async {
    await pumpApp(tester);
    await tester.tap(find.byIcon(Icons.info_outline));
    await tester.pumpAndSettle();

    expect(find.text('Version'), findsOneWidget);
    expect(find.text('dev'), findsOneWidget);
    expect(find.text('local'), findsOneWidget);
  });
}
