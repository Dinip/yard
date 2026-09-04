import 'dart:async';

import 'incoming_share.dart';
import 'share_gateway.dart';

/// The Android-backed gateway. Increment 3 connects it to the method and event
/// channels; until then it behaves like a device that has received nothing, so
/// the app runs end to end on top of the real seam rather than a test double.
final class PlatformShareGateway implements ShareGateway {
  @override
  Stream<IncomingShare> get changes => const Stream.empty();

  @override
  Future<List<IncomingShare>> pendingShares() async => const [];

  @override
  Future<SaveResult> save(String shareId, ShareDestination destination) async {
    throw UnimplementedError('the Android save path lands in increment 5');
  }

  @override
  Future<void> discard(String shareId) async {}
}
