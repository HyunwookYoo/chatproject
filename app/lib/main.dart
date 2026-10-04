import 'package:flutter/material.dart';

import 'src/rust/api/chat.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  await RustLib.init();
  runApp(MaterialApp(home: Scaffold(body: Center(child: Text('core ${coreVersion()}')))));
}
