import 'incoming_share.dart';

/// The boundary between Dart and the native share receiver. File contents never
/// cross it — only metadata and the user's choice of destination.
abstract interface class ShareGateway {
  Future<List<IncomingShare>> pendingShares();

  Stream<IncomingShare> get changes;

  Future<SaveResult> save(String shareId, ShareDestination destination);

  Future<void> discard(String shareId);
}
