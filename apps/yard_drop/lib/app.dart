import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';

import 'share/ios_share_page.dart';
import 'share/share_gateway.dart';
import 'share/share_page.dart';
import 'theme.dart';

class YardDropApp extends StatelessWidget {
  const YardDropApp({required this.gateway, super.key});

  final ShareGateway gateway;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'YARD - Device Farm',
      theme: yardTheme(Brightness.light),
      darkTheme: yardTheme(Brightness.dark),
      home: defaultTargetPlatform == TargetPlatform.iOS
          ? const IosSharePage()
          : SharePage(gateway: gateway),
    );
  }
}
