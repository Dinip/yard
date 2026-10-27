import 'package:flutter/material.dart';

import '../about_page.dart';

class IosSharePage extends StatelessWidget {
  const IosSharePage({super.key});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('YARD - Device Farm'),
        actions: [
          IconButton(
            icon: const Icon(Icons.info_outline),
            tooltip: 'About',
            onPressed: () => Navigator.of(
              context,
            ).push(MaterialPageRoute<void>(builder: (_) => const AboutPage())),
          ),
        ],
      ),
      body: const SafeArea(
        child: Center(
          child: Padding(
            padding: EdgeInsets.all(24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text('Share a file to YARD'),
                SizedBox(height: 16),
                Text(
                  'In another app, share files and choose YARD - Device Farm. '
                  'Choose Save on this device to pick a location in Files. '
                  'During a YARD reservation, choose Send to YARD browser '
                  'to receive them in your browser automatically.',
                  textAlign: TextAlign.center,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
