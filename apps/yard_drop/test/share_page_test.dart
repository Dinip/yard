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
    expect(find.text('Save to Downloads'), findsNothing);
  });

  testWidgets('one ready file shows its name, size and type', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    expect(find.text('File ready'), findsOneWidget);
    expect(find.text('build.apk'), findsOneWidget);
    expect(find.textContaining('4.2 MB'), findsOneWidget);
    expect(find.text('Save to Downloads'), findsOneWidget);
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

  testWidgets('the browser destination is not offered yet', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    expect(find.textContaining('browser'), findsNothing);
  });

  testWidgets('saving shows progress until it completes', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    final gate = Completer<SaveResult>();
    gateway.onSave = (_, _) => gate.future;
    await tester.tap(find.text('Save to Downloads'));
    await tester.pump();

    expect(find.text('Saving'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);

    gate.complete(const SaveResult.success(ShareDestination.downloads));
    await tester.pumpAndSettle();

    expect(find.text('Saved'), findsOneWidget);
    expect(find.textContaining('build.apk is in Downloads'), findsOneWidget);
  });

  testWidgets('done clears the share and returns to empty', (tester) async {
    gateway.emit(shareFixture());
    await pumpApp(tester);

    await tester.tap(find.text('Save to Downloads'));
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

    await tester.tap(find.text('Save to Downloads'));
    await tester.pumpAndSettle();

    expect(find.text('Not saved'), findsOneWidget);
    expect(find.text('no space left on device'), findsOneWidget);

    gateway.onSave = null;
    await tester.tap(find.text('Try again'));
    await tester.pumpAndSettle();
    expect(find.text('File ready'), findsOneWidget);

    await tester.tap(find.text('Save to Downloads'));
    await tester.pumpAndSettle();
    expect(find.text('Saved'), findsOneWidget);
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
