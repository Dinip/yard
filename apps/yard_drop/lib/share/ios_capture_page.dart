import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../about_page.dart';

const _channel = MethodChannel('com.dinispimpao.yard.drop/capture');

class IosCapturePage extends StatefulWidget {
  const IosCapturePage({super.key});

  @override
  State<IosCapturePage> createState() => _IosCapturePageState();
}

class _IosCapturePageState extends State<IosCapturePage> {
  late final _status = _channel.invokeMapMethod<String, Object?>(
    'captureStatus',
  );

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
      body: SafeArea(
        child: Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: FutureBuilder(
              future: _status,
              builder: (context, snapshot) {
                if (snapshot.connectionState != ConnectionState.done) {
                  return const CircularProgressIndicator();
                }
                final status = snapshot.data;
                final state = status?['state'];
                final (title, message) = switch (state) {
                  'armed' => (
                    'Ready to receive',
                    'Return to the app you are testing, share files and choose '
                        'YARD - Device Farm, then Send to YARD browser. Select '
                        'Finish transfer in YARD when you are done.',
                  ),
                  'published' => (
                    'Ready for the browser',
                    '${status?['count']} file(s) are ready. Return to your YARD '
                        'browser to download them.',
                  ),
                  'empty' => (
                    'No files waiting',
                    'Share files to YARD - Device Farm and choose Send to YARD '
                        'browser, then select Finish transfer again.',
                  ),
                  'failed' => (
                    'Transfer failed',
                    status?['error'] as String? ??
                        'Prepare the iPhone again in YARD.',
                  ),
                  _ when snapshot.hasError => (
                    'Transfer failed',
                    'Close the app and prepare the iPhone again in YARD.',
                  ),
                  _ => (
                    'Share a file to YARD',
                    'Share files from another app and choose YARD - Device Farm '
                        'to save them through Files. To send them to your browser, '
                        'open Receive shared files in YARD and prepare the iPhone first.',
                  ),
                };
                return Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      title,
                      style: Theme.of(context).textTheme.headlineSmall,
                    ),
                    const SizedBox(height: 16),
                    Text(message, textAlign: TextAlign.center),
                  ],
                );
              },
            ),
          ),
        ),
      ),
    );
  }
}
