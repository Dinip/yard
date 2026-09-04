import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/app.dart';

void main() {
  testWidgets('home explains how a share reaches the app', (tester) async {
    await tester.pumpWidget(const YardDropApp());

    expect(find.text('Nothing to drop yet'), findsOneWidget);
    expect(find.textContaining('Share a file from any app'), findsOneWidget);
  });

  testWidgets('about shows the build identity', (tester) async {
    await tester.pumpWidget(const YardDropApp());
    await tester.tap(find.byIcon(Icons.info_outline));
    await tester.pumpAndSettle();

    expect(find.text('Version'), findsOneWidget);
    expect(find.text('Commit'), findsOneWidget);
    expect(find.text('dev'), findsOneWidget);
  });
}
