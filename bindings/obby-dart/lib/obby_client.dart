/// The Obby IRCv3 client engine, over its C ABI.
///
/// The engine opens no socket and reads no clock. A host feeds it bytes and time, and drains bytes,
/// events and the next deadline back out. Every method here mirrors one on the Rust side.
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import 'src/bindings.dart';

/// How far along a composed message is.
enum TypingState {
  /// Typing right now.
  active,

  /// Stopped, with text still in the box.
  paused,

  /// Stopped, with the box empty.
  done;

  String get _wire => name;
}

/// One connection.
///
/// Call [close] when finished. The engine holds native memory that Dart's collector knows nothing
/// about, so nothing releases it on your behalf.
///
/// One client belongs to one isolate. The native handle is not synchronised, so sending its address
/// to another isolate and rebuilding it there is undefined behaviour, not merely a race. Give each
/// isolate its own client.
class ObbyClient {
  ObbyClient._(this._bindings, this._handle);

  final Bindings _bindings;
  Pointer<ObbyClientHandle> _handle;

  /// Open a connection's engine.
  ///
  /// Only `nick` is required in [config]; every other field has a default.
  factory ObbyClient(Map<String, dynamic> config, {String? libraryPath}) {
    final bindings = Bindings(_load(libraryPath));
    final json = jsonEncode(config).toNativeUtf8();
    try {
      final handle = bindings.newClient(json);
      if (handle == nullptr) {
        throw ArgumentError.value(config, 'config', 'the engine refused it');
      }
      return ObbyClient._(bindings, handle);
    } finally {
      calloc.free(json);
    }
  }

  static DynamicLibrary _load(String? path) {
    if (path != null) {
      return DynamicLibrary.open(path);
    }
    final name = Platform.isWindows
        ? 'obby_ffi.dll'
        : Platform.isMacOS
        ? 'libobby_ffi.dylib'
        : 'libobby_ffi.so';
    for (final profile in ['release', 'debug']) {
      final candidate = '../../target/$profile/$name';
      if (File(candidate).existsSync()) {
        return DynamicLibrary.open(candidate);
      }
    }
    return DynamicLibrary.open(name);
  }

  void _alive() {
    if (_handle == nullptr) {
      throw StateError('this client is closed');
    }
  }

  /// Tell the engine the transport is up. Queues the registration burst.
  void handleConnected() {
    _alive();
    _bindings.handleConnected(_handle);
  }

  /// Tell the engine its transport died. The model survives, so a reconnect resumes from it.
  void handleDisconnected() {
    _alive();
    _bindings.handleDisconnected(_handle);
  }

  /// Feed whatever the transport read. Partial lines are held until the rest arrives.
  void handleBytes(Uint8List data) {
    _alive();
    if (data.isEmpty) {
      return;
    }
    final buffer = calloc<Uint8>(data.length);
    try {
      buffer.asTypedList(data.length).setAll(0, data);
      _bindings.handleBytes(_handle, buffer, data.length);
    } finally {
      calloc.free(buffer);
    }
  }

  /// Bytes to write to the transport, or null when there are none.
  Uint8List? pollTransmit() {
    _alive();
    final bytes = _bindings.pollTransmit(_handle);
    if (bytes.ptr == nullptr || bytes.len == 0) {
      return null;
    }
    // copy before freeing: the returned list would otherwise view memory we are about to release
    final copy = Uint8List.fromList(bytes.ptr.asTypedList(bytes.len));
    _bindings.freeBytes(bytes);
    return copy;
  }

  /// Every event queued since the last call.
  ///
  /// One crossing per drain rather than one per event, because a call over this boundary costs the
  /// same whether it carries one event or a hundred.
  List<Map<String, dynamic>> pollEvents() {
    _alive();
    final decoded = _takeString(_bindings.pollEvents(_handle));
    if (decoded == null) {
      return const [];
    }
    final parsed = jsonDecode(decoded);
    if (parsed is! List) {
      return const [];
    }
    return parsed.whereType<Map<String, dynamic>>().toList();
  }

  /// Everything the connection knows: channels, members, conversations, messages.
  Map<String, dynamic> model() {
    _alive();
    final decoded = _takeString(_bindings.modelJson(_handle));
    if (decoded == null) {
      return const {};
    }
    final parsed = jsonDecode(decoded);
    return parsed is Map<String, dynamic> ? parsed : const {};
  }

  /// Advance the clock. [monotonicMs] drives every deadline; [unixMs] only stamps a message the
  /// server did not stamp itself.
  void tick(int monotonicMs, int unixMs) {
    _alive();
    _bindings.tick(_handle, monotonicMs, unixMs);
  }

  /// When [tick] next has something to do, or null when nothing is scheduled.
  int? pollTimeout() {
    _alive();
    final out = calloc<Uint64>();
    try {
      return _bindings.pollTimeout(_handle, out) ? out.value : null;
    } finally {
      calloc.free(out);
    }
  }

  /// Do something on this connection. Returns false when the command could not be read.
  bool command(Map<String, dynamic> command) {
    _alive();
    final json = jsonEncode(command).toNativeUtf8();
    try {
      return _bindings.command(_handle, json);
    } finally {
      calloc.free(json);
    }
  }

  /// Join a channel.
  bool join(String channel, {String? key}) {
    _alive();
    return command({'type': 'join', 'channel': channel, 'key': key});
  }

  /// Leave a channel.
  bool part(String channel, {String? reason}) {
    _alive();
    return command({'type': 'part', 'channel': channel, 'reason': reason});
  }

  /// Say something to a channel or a person.
  bool sendMessage(String target, String text) {
    _alive();
    return command({'type': 'send_message', 'target': target, 'text': text});
  }

  /// Send a notice, which by convention must never be auto-replied to.
  bool sendNotice(String target, String text) {
    _alive();
    return command({'type': 'send_notice', 'target': target, 'text': text});
  }

  /// Send a `CTCP ACTION`, the third-person form.
  bool sendAction(String target, String text) {
    _alive();
    return command({'type': 'send_action', 'target': target, 'text': text});
  }

  /// Change our nick.
  bool setNick(String nick) {
    _alive();
    return command({'type': 'set_nick', 'nick': nick});
  }

  /// Set or clear a channel topic.
  bool setTopic(String channel, {String? topic}) {
    _alive();
    return command({'type': 'set_topic', 'channel': channel, 'topic': topic});
  }

  /// Mark ourselves away, or come back.
  bool setAway({String? message}) {
    _alive();
    return command({'type': 'set_away', 'message': message});
  }

  /// Say we are typing, so others can show it.
  bool setTyping(String target, TypingState state) {
    _alive();
    return command({'type': 'set_typing', 'target': target, 'state': state._wire});
  }

  /// React to a message with an emoji.
  bool addReaction(String target, String msgid, String emoji) {
    _alive();
    return command({'type': 'add_reaction', 'target': target, 'msgid': msgid, 'emoji': emoji});
  }

  /// Take a reaction back.
  bool removeReaction(String target, String msgid, String emoji) {
    _alive();
    return command({'type': 'remove_reaction', 'target': target, 'msgid': msgid, 'emoji': emoji});
  }

  /// Ask the server to delete a message.
  bool redactMessage(String target, String msgid, {String? reason}) {
    _alive();
    return command({
      'type': 'redact_message',
      'target': target,
      'msgid': msgid,
      'reason': reason,
    });
  }

  /// Tell the server how far we have read.
  bool markRead(String target, String timestamp) {
    _alive();
    return command({'type': 'mark_read', 'target': target, 'timestamp': timestamp});
  }

  /// Ask for older messages than the ones we hold.
  ///
  /// With no [before], this asks for the most recent, which is what a fresh window wants.
  bool fetchHistory(String target, {String? before, int limit = 50}) {
    _alive();
    return command({
      'type': 'fetch_history',
      'target': target,
      'before': before,
      'limit': limit,
    });
  }

  /// Set one of our own metadata keys, or clear it.
  bool setMetadata(String key, {String? value}) {
    _alive();
    return command({'type': 'set_metadata', 'key': key, 'value': value});
  }

  /// Ask to be told when these metadata keys change on anyone we can see.
  bool subscribeMetadata(List<String> keys) {
    _alive();
    return command({'type': 'subscribe_metadata', 'keys': keys});
  }

  /// Watch these nicks, so the server says when they come and go.
  bool watchNicks(List<String> nicks) {
    _alive();
    return command({'type': 'watch_nicks', 'nicks': nicks});
  }

  /// Stop watching these nicks.
  bool unwatchNicks(List<String> nicks) {
    _alive();
    return command({'type': 'unwatch_nicks', 'nicks': nicks});
  }

  /// Send a voice signalling frame to a room.
  ///
  /// The frame is the caller's to build: everything in it comes from a media stack the engine
  /// deliberately knows nothing about.
  bool sendVoiceSignal(String channel, String signalJson) {
    _alive();
    return command({
      'type': 'send_voice_signal',
      'channel': channel,
      'signal_json': signalJson,
    });
  }

  /// Leave the network.
  bool quit({String? reason}) {
    _alive();
    return command({'type': 'quit', 'reason': reason});
  }

  /// Send a line the engine does not model. The escape hatch, so you are never stuck waiting on it.
  bool sendRawLine(String line) {
    _alive();
    return command({'type': 'send_raw_line', 'line': line});
  }

  /// The engine's version.
  String get version => _bindings.version().toDartString();

  /// Release the engine. Safe to call more than once.
  void close() {
    if (_handle != nullptr) {
      _bindings.freeClient(_handle);
      _handle = nullptr;
    }
  }

  /// Read a string the engine allocated, then hand it back to the engine to free.
  ///
  /// The engine's allocator is not Dart's, so releasing this any other way corrupts the heap.
  String? _takeString(Pointer<Utf8> pointer) {
    if (pointer == nullptr) {
      return null;
    }
    final value = pointer.toDartString();
    _bindings.freeString(pointer);
    return value;
  }
}
