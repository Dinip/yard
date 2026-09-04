import 'dart:async';

import 'package:flutter/foundation.dart';

import 'incoming_share.dart';
import 'share_gateway.dart';

/// Owns what the screen shows: the shares the native side has staged, and which
/// one the user is currently answering for.
///
/// Shares are queued rather than replaced. A user can share twice before
/// answering the first prompt, and dropping the earlier one would silently lose
/// a file the native side has already copied out of its temporary URI grant.
final class ShareController extends ChangeNotifier {
  ShareController(this._gateway);

  final ShareGateway _gateway;
  StreamSubscription<IncomingShare>? _subscription;
  final List<IncomingShare> _queue = [];

  /// The share the user is answering for, oldest first.
  IncomingShare? get current => _queue.isEmpty ? null : _queue.first;

  int get waiting => _queue.length;

  Future<void> start() async {
    // Subscribe before the query: an event that lands during it is then a
    // duplicate of a queued share rather than a lost one.
    _subscription ??= _gateway.changes.listen(_apply);
    await refresh();
  }

  /// Reconciles against the native side. Called on startup and on resume,
  /// because an event emitted while Dart was not listening is simply gone.
  Future<void> refresh() async {
    for (final share in await _gateway.pendingShares()) {
      _apply(share, notify: false);
    }
    notifyListeners();
  }

  Future<void> save(ShareDestination destination) async {
    final share = current;
    if (share == null || share.state != IncomingShareState.ready) return;

    _replace(
      share.copyWith(state: IncomingShareState.saving, clearError: true),
    );

    final SaveResult result;
    try {
      result = await _gateway.save(share.id, destination);
    } catch (error) {
      _replace(
        share.copyWith(state: IncomingShareState.failed, error: '$error'),
      );
      return;
    }

    _replace(
      result.succeeded
          ? share.copyWith(state: IncomingShareState.saved, clearError: true)
          : share.copyWith(
              state: IncomingShareState.failed,
              error: result.error,
            ),
    );
  }

  /// Puts a failed share back where the user can choose a destination again.
  void retry() {
    final share = current;
    if (share?.state != IncomingShareState.failed) return;
    _replace(
      share!.copyWith(state: IncomingShareState.ready, clearError: true),
    );
  }

  /// Answers for the current share and moves on to the next one.
  Future<void> dismiss() async {
    final share = current;
    if (share == null) return;

    _queue.removeAt(0);
    notifyListeners();
    await _gateway.discard(share.id);
  }

  void _apply(IncomingShare share, {bool notify = true}) {
    final index = _queue.indexWhere((queued) => queued.id == share.id);
    if (index == -1) {
      _queue.add(share);
    } else {
      // A save the controller is driving owns the state; a native event for the
      // same share must not walk it backwards to `ready`.
      final known = _queue[index];
      if (known.state == IncomingShareState.saving &&
          share.state == IncomingShareState.ready) {
        return;
      }
      _queue[index] = share;
    }
    if (notify) notifyListeners();
  }

  void _replace(IncomingShare share) {
    final index = _queue.indexWhere((queued) => queued.id == share.id);
    if (index == -1) return;
    _queue[index] = share;
    notifyListeners();
  }

  @override
  void dispose() {
    unawaited(_subscription?.cancel());
    super.dispose();
  }
}
