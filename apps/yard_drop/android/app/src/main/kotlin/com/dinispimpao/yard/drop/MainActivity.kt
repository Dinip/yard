package com.dinispimpao.yard.drop

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    private var bridge: IncomingShareBridge? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val bridge = IncomingShareBridge(applicationContext, flutterEngine.dartExecutor.binaryMessenger)
        this.bridge = bridge
        // A cold-start share is already on the intent before Dart can subscribe,
        // which is why the store, not the event, is what Dart reads first.
        bridge.receive(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        bridge?.receive(intent)
    }

    override fun onDestroy() {
        bridge?.dispose()
        bridge = null
        super.onDestroy()
    }
}
