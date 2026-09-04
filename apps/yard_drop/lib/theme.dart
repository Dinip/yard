import 'package:flutter/material.dart';

const _seed = Color(0xFF3B6EA5);

ThemeData yardTheme(Brightness brightness) {
  final colors = ColorScheme.fromSeed(seedColor: _seed, brightness: brightness);

  return ThemeData(
    colorScheme: colors,
    useMaterial3: true,
    appBarTheme: AppBarTheme(
      backgroundColor: colors.surface,
      foregroundColor: colors.onSurface,
      centerTitle: false,
    ),
  );
}
