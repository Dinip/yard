/// Build identity, supplied by CI so an installed APK can be traced back to a
/// release and a commit. `flutter run` leaves the defaults in place.
final class BuildInfo {
  const BuildInfo({
    required this.version,
    required this.buildNumber,
    required this.commit,
  });

  static const BuildInfo current = BuildInfo(
    version: String.fromEnvironment('YARD_VERSION', defaultValue: 'dev'),
    buildNumber: String.fromEnvironment('YARD_BUILD_NUMBER', defaultValue: '0'),
    commit: String.fromEnvironment('YARD_COMMIT', defaultValue: 'local'),
  );

  final String version;
  final String buildNumber;
  final String commit;

  String get display => '$version+$buildNumber ($commit)';
}
