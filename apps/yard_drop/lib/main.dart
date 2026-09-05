import 'package:flutter/material.dart';

import 'app.dart';
import 'share/platform_share_gateway.dart';

void main() {
  runApp(YardDropApp(gateway: PlatformShareGateway()));
}
