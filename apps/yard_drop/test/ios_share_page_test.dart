import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yard_drop/share/ios_share_page.dart';

void main() {
  testWidgets('iOS home explains automatic sharing', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: IosSharePage()));
    expect(find.text('Share a file to YARD'), findsOneWidget);
    expect(find.textContaining('automatically'), findsOneWidget);
    expect(find.byTooltip('About'), findsOneWidget);
  });
}
