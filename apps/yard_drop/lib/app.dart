import 'package:flutter/material.dart';

import 'home_page.dart';
import 'theme.dart';

class YardDropApp extends StatelessWidget {
  const YardDropApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'YARD - Device Farm',
      theme: yardTheme(Brightness.light),
      darkTheme: yardTheme(Brightness.dark),
      home: const HomePage(),
    );
  }
}
