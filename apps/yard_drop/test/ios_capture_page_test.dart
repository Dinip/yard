import 'package:flutter/services.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/share/ios_capture_page.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('com.dinispimpao.yard.drop/capture');

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, null);
  });

  for (final (state, expected) in [
    ('idle', 'Share a file to YARD'),
    ('armed', 'Ready to receive'),
    ('published', 'Ready for the browser'),
    ('empty', 'No files waiting'),
    ('failed', 'Transfer failed'),
  ]) {
    testWidgets('shows the $state launch result', (tester) async {
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            expect(call.method, 'captureStatus');
            return {'state': state, 'count': 2, 'error': 'Prepare again'};
          });
      await tester.pumpWidget(const MaterialApp(home: IosCapturePage()));
      await tester.pumpAndSettle();
      expect(find.text(expected), findsOneWidget);
      if (state == 'failed') expect(find.text('Prepare again'), findsOneWidget);
      expect(find.text('Save on this device'), findsNothing);
    });
  }
}
