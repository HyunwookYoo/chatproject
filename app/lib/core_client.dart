import 'dart:io';

import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api/chat.dart' as rust;
import 'network/network_client.dart';
import 'account/account_client.dart';
import 'message/message_client.dart';

const _accountStorage = MethodChannel('chat_app/account_storage');

/// Paths and display capabilities may cross this channel; key material never does.
Future<String> accountStorageRoot(String existing) async {
  if (Platform.isAndroid || Platform.isIOS) {
    final root = await _accountStorage.invokeMethod<String>('storageRoot', {
      'existing': existing,
    });
    if (root == null || root.isEmpty) throw StateError('계정 저장 공간을 열 수 없어요.');
    return root;
  }
  if (Platform.isWindows) {
    for (final name in [
      'store.id',
      'key.wrap',
      'account.bin',
      '.store.id.tmp',
      '.key.wrap.tmp',
      '.account.bin.tmp',
      // Preserve the path; Rust alone validates incomplete SQL material.
      'db.sqlite3',
      'db.sqlite3-wal',
      'db.sqlite3-shm',
      'db.keys',
      'db.keys.pending',
      '.db.keys.tmp',
      '.db.keys.pending.tmp',
      'storage.mode',
      '.storage.mode.tmp',
      'migration.prep',
      '.migration.prep.tmp',
      'migration.checkpoint',
      '.migration.checkpoint.tmp',
    ]) {
      if (await FileSystemEntity.type(
            '$existing${Platform.pathSeparator}$name',
          ) !=
          FileSystemEntityType.notFound) {
        return existing;
      }
    }
    final local = Platform.environment['LOCALAPPDATA'];
    if (local == null || local.isEmpty) throw StateError('계정 저장 공간을 열 수 없어요.');
    return '$local\\ChatProject\\account';
  }
  // The Rust provider returns NotAvailable on unsupported OSes; the account
  // model keeps that failure on its locked route without a fake protector.
  return existing;
}

Future<void> setSensitiveScreen(bool sensitive) async {
  if (Platform.isAndroid || Platform.isIOS || Platform.isWindows) {
    await _accountStorage.invokeMethod<void>('setSensitiveScreen', sensitive);
  }
}

/// The UI reaches the Rust core only through this interface, so widget tests can use
/// a fake core.
abstract interface class CoreClient {
  String get version;
  Future<rust.CoreInfo> start();
  Stream<rust.ChatEvent> events();
}

class FrbCoreClient implements CoreClient, NetworkClient {
  FrbCoreClient({this.dataDir});

  final String? dataDir;
  late final AccountClient account = FrbAccountClient(this);
  late final MessageClient messages = FrbMessageClient(this);
  Stream<rust.ChatEvent>? _events;

  @override
  String get version => rust.coreVersion();

  @override
  Future<rust.CoreInfo> start() async {
    if (dataDir != null) return rust.coreStart(dataDir: dataDir!);
    final support = await getApplicationSupportDirectory();
    final root = await accountStorageRoot(
      '${support.path}${Platform.pathSeparator}core',
    );
    return rust.coreStart(dataDir: root);
  }

  @override
  // Keep one Rust subscription across page listeners, without replaying messages.
  Stream<rust.ChatEvent> events() =>
      _events ??= rust.events().asBroadcastStream();

  @override
  Future<rust.NetworkInfo> startNetwork() => rust.networkStart();
  @override
  Future<rust.NetworkInfo> networkInfo() => rust.networkInfo();
  @override
  Future<rust.PeerInfo> connectPeer(String target) =>
      rust.connectPeer(target: target);
  @override
  Future<void> sendProbeText(String peerId, String text) =>
      rust.sendProbeText(peerId: peerId, text: text);
  @override
  Future<void> disconnectPeer(String peerId) =>
      rust.disconnectPeer(peerId: peerId);
  @override
  Future<void> stopNetwork() => rust.networkStop();
}
