import 'dart:async';

import 'package:yard_drop/share/incoming_share.dart';
import 'package:yard_drop/share/share_gateway.dart';

/// An in-memory [ShareGateway] so the screens and controller can be tested
/// without an Android device.
final class FakeShareGateway implements ShareGateway {
  final _controller = StreamController<IncomingShare>.broadcast();
  final _shares = <String, IncomingShare>{};
  final savedDestinations = <String, ShareDestination>{};
  final discarded = <String>[];

  /// Lets a test decide the outcome, and control when it arrives.
  Future<SaveResult> Function(String shareId, ShareDestination destination)?
  onSave;

  @override
  Stream<IncomingShare> get changes => _controller.stream;

  @override
  Future<List<IncomingShare>> pendingShares() async => _shares.values.toList();

  @override
  Future<SaveResult> save(String shareId, ShareDestination destination) async {
    final result =
        await onSave?.call(shareId, destination) ??
        SaveResult.success(destination);
    if (result.succeeded) {
      savedDestinations[shareId] = destination;
      emit(_shares[shareId]!.copyWith(state: IncomingShareState.saved));
    } else {
      emit(
        _shares[shareId]!.copyWith(
          state: IncomingShareState.failed,
          error: result.error,
        ),
      );
    }
    return result;
  }

  @override
  Future<void> discard(String shareId) async {
    _shares.remove(shareId);
    discarded.add(shareId);
  }

  /// Stands in for a native share arriving.
  void emit(IncomingShare share) {
    _shares[share.id] = share;
    _controller.add(share);
  }

  Future<void> dispose() => _controller.close();
}
