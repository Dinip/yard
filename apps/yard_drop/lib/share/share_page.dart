import 'package:flutter/material.dart';

import '../about_page.dart';
import 'incoming_share.dart';
import 'share_controller.dart';
import 'share_gateway.dart';

class SharePage extends StatefulWidget {
  const SharePage({required this.gateway, super.key});

  final ShareGateway gateway;

  @override
  State<SharePage> createState() => _SharePageState();
}

class _SharePageState extends State<SharePage> {
  late final ShareController _controller = ShareController(widget.gateway);
  late final AppLifecycleListener _lifecycle;

  @override
  void initState() {
    super.initState();
    // A share that arrived while the app was backgrounded produced an event
    // nobody was listening for; resuming re-reads the native side instead.
    _lifecycle = AppLifecycleListener(onResume: _controller.refresh);
    _controller.start();
  }

  @override
  void dispose() {
    _lifecycle.dispose();
    _controller.dispose();
    super.dispose();
  }

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
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 440),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: ListenableBuilder(
                listenable: _controller,
                builder: (context, _) => _body(context),
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _body(BuildContext context) {
    final share = _controller.current;
    if (share == null) return const _EmptyState();

    return switch (share.state) {
      IncomingShareState.receiving => _ReceivingState(share: share),
      IncomingShareState.ready => _ReadyState(
        share: share,
        waiting: _controller.waiting,
        onSave: () => _controller.save(ShareDestination.downloads),
        onDiscard: _controller.dismiss,
      ),
      IncomingShareState.saving => _SavingState(share: share),
      IncomingShareState.saved => _SavedState(
        share: share,
        onDone: _controller.dismiss,
      ),
      IncomingShareState.failed => _FailedState(
        share: share,
        onRetry: _controller.retry,
        onDiscard: _controller.dismiss,
      ),
    };
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState();

  @override
  Widget build(BuildContext context) {
    return _Panel(
      icon: Icons.ios_share,
      title: 'Nothing to drop yet',
      message:
          'Share a file from any app and choose YARD - Device Farm. It lands '
          'here, and you pick where it goes: this device, or the browser you '
          'reserved it from.',
    );
  }
}

class _ReceivingState extends StatelessWidget {
  const _ReceivingState({required this.share});

  final IncomingShare share;

  @override
  Widget build(BuildContext context) {
    return _Panel(
      progress: true,
      title: 'Receiving',
      message: share.files.isEmpty
          ? 'Copying the attachment from the app that shared it.'
          : 'Copying ${_fileCount(share.files.length)} from the app that '
                'shared them.',
    );
  }
}

class _ReadyState extends StatelessWidget {
  const _ReadyState({
    required this.share,
    required this.waiting,
    required this.onSave,
    required this.onDiscard,
  });

  final IncomingShare share;
  final int waiting;
  final VoidCallback onSave;
  final VoidCallback onDiscard;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          share.isBatch
              ? '${_fileCount(share.files.length)} ready'
              : 'File ready',
          style: theme.textTheme.headlineSmall,
        ),
        if (waiting > 1) ...[
          const SizedBox(height: 4),
          Text(
            '${waiting - 1} more share${waiting == 2 ? '' : 's'} waiting',
            style: theme.textTheme.bodySmall,
          ),
        ],
        const SizedBox(height: 16),
        Flexible(child: _FileList(files: share.files)),
        const SizedBox(height: 24),
        FilledButton.icon(
          onPressed: onSave,
          icon: const Icon(Icons.download),
          label: const Text('Save to Downloads'),
        ),
        const SizedBox(height: 8),
        TextButton(onPressed: onDiscard, child: const Text('Discard')),
      ],
    );
  }
}

class _SavingState extends StatelessWidget {
  const _SavingState({required this.share});

  final IncomingShare share;

  @override
  Widget build(BuildContext context) {
    return _Panel(
      progress: true,
      title: 'Saving',
      message: share.isBatch
          ? 'Writing ${_fileCount(share.files.length)} to Downloads.'
          : 'Writing ${share.files.first.displayName} to Downloads.',
    );
  }
}

class _SavedState extends StatelessWidget {
  const _SavedState({required this.share, required this.onDone});

  final IncomingShare share;
  final VoidCallback onDone;

  @override
  Widget build(BuildContext context) {
    return _Panel(
      icon: Icons.check_circle_outline,
      title: 'Saved',
      message: share.isBatch
          ? '${_fileCount(share.files.length)} are in Downloads.'
          : '${share.files.first.displayName} is in Downloads.',
      action: FilledButton(onPressed: onDone, child: const Text('Done')),
    );
  }
}

class _FailedState extends StatelessWidget {
  const _FailedState({
    required this.share,
    required this.onRetry,
    required this.onDiscard,
  });

  final IncomingShare share;
  final VoidCallback onRetry;
  final VoidCallback onDiscard;

  @override
  Widget build(BuildContext context) {
    return _Panel(
      icon: Icons.error_outline,
      danger: true,
      title: 'Not saved',
      message: share.error ?? 'The files could not be written to Downloads.',
      action: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          FilledButton(onPressed: onRetry, child: const Text('Try again')),
          TextButton(onPressed: onDiscard, child: const Text('Discard')),
        ],
      ),
    );
  }
}

class _FileList extends StatelessWidget {
  const _FileList({required this.files});

  final List<IncomingFile> files;

  @override
  Widget build(BuildContext context) {
    return ListView.separated(
      shrinkWrap: true,
      itemCount: files.length,
      separatorBuilder: (_, _) => const Divider(height: 1),
      itemBuilder: (context, index) {
        final file = files[index];
        return ListTile(
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.insert_drive_file_outlined),
          title: Text(file.displayName, overflow: TextOverflow.ellipsis),
          subtitle: Text(_describe(file)),
        );
      },
    );
  }
}

class _Panel extends StatelessWidget {
  const _Panel({
    required this.title,
    required this.message,
    this.icon,
    this.progress = false,
    this.danger = false,
    this.action,
  });

  final String title;
  final String message;
  final IconData? icon;
  final bool progress;
  final bool danger;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tint = danger ? theme.colorScheme.error : theme.colorScheme.primary;

    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        if (progress)
          const SizedBox(
            width: 48,
            height: 48,
            child: CircularProgressIndicator(),
          )
        else if (icon != null)
          Icon(icon, size: 64, color: tint),
        const SizedBox(height: 24),
        Text(
          title,
          style: theme.textTheme.headlineSmall,
          textAlign: TextAlign.center,
        ),
        const SizedBox(height: 12),
        Text(
          message,
          style: theme.textTheme.bodyMedium,
          textAlign: TextAlign.center,
        ),
        if (action != null) ...[const SizedBox(height: 24), action!],
      ],
    );
  }
}

String _fileCount(int count) => count == 1 ? '1 file' : '$count files';

String _describe(IncomingFile file) {
  final parts = [
    if (file.reportedSize != null) formatBytes(file.reportedSize!),
    file.mimeType ?? 'unknown type',
  ];
  return parts.join(' · ');
}

/// Metadata is what the sharing app claimed, so this is a hint, not a promise.
String formatBytes(int bytes) {
  const units = ['B', 'kB', 'MB', 'GB'];
  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  final rounded = unit == 0 || value >= 100
      ? value.round().toString()
      : value.toStringAsFixed(1);
  return '$rounded ${units[unit]}';
}
