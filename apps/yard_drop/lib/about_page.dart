import 'package:flutter/material.dart';

import 'build_info.dart';

class AboutPage extends StatelessWidget {
  const AboutPage({super.key});

  @override
  Widget build(BuildContext context) {
    const info = BuildInfo.current;

    return Scaffold(
      appBar: AppBar(title: const Text('About')),
      body: ListView(
        padding: const EdgeInsets.symmetric(vertical: 8),
        children: [
          const ListTile(
            title: Text('YARD - Device Farm'),
            subtitle: Text('Share companion'),
          ),
          const Divider(),
          ListTile(title: Text('Version'), subtitle: Text(info.version)),
          ListTile(title: Text('Build'), subtitle: Text(info.buildNumber)),
          ListTile(title: Text('Commit'), subtitle: Text(info.commit)),
        ],
      ),
    );
  }
}
