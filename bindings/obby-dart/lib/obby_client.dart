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

/// SASL credentials for authenticating during registration.
class SaslCredentials {
  SaslCredentials._(this._json);

  final Map<String, dynamic> _json;

  /// `PLAIN`: a username and password sent in the clear. Only safe once the connection is over
  /// TLS, since nothing else protects them.
  factory SaslCredentials.plain({required String username, required String password}) {
    return SaslCredentials._({
      'mechanism': 'plain',
      'username': username,
      'password': password,
    });
  }

  /// `EXTERNAL`: authenticates with the TLS client certificate already on the connection, so no
  /// password is needed at all.
  factory SaslCredentials.external() =>
      SaslCredentials._({'mechanism': 'external'});

  /// `SCRAM-SHA-256`: proves the password without ever putting it on the wire. Preferred over
  /// [SaslCredentials.plain] wherever the server offers it.
  factory SaslCredentials.scram({
    required String username,
    required String password,
    required String nonce,
  }) {
    return SaslCredentials._({
      'mechanism': 'scram',
      'username': username,
      'password': password,
      'nonce': nonce,
    });
  }

  /// The wire shape the engine's `Credentials` type expects.
  Map<String, dynamic> toJson() => _json;
}

/// One connection.
///
/// Call [close] when finished. The engine holds native memory that Dart's collector knows nothing
/// about, so nothing releases it on your behalf.
///
/// One client belongs to one isolate. The native handle is not synchronised, so sending its address
/// to another isolate and rebuilding it there is undefined behaviour, not merely a race. Give each
/// isolate its own client.
///
/// ```dart
/// final client = ObbyClient(nick: 'me');
/// final socket = await Socket.connect('irc.libera.chat', 6667);
/// client.handleConnected();
///
/// socket.listen((chunk) {
///   client.handleBytes(chunk);
///   client.tick(stopwatch.elapsedMilliseconds, DateTime.now().millisecondsSinceEpoch);
///
///   for (final event in client.pollEvents()) {
///     if (event['type'] == 'registered') {
///       client.join('#obby');
///       client.sendMessage('#obby', 'hello');
///     }
///   }
///   for (var out = client.pollTransmit(); out != null; out = client.pollTransmit()) {
///     socket.add(out);
///   }
/// });
/// ```
class ObbyClient {
  ObbyClient._(this._bindings, this._handle);

  final Bindings _bindings;
  Pointer<ObbyClientHandle> _handle;

  /// Open a connection's engine.
  ///
  /// Only [nick] is required; the engine defaults everything else.
  factory ObbyClient({
    required String nick,
    String? username,
    String? realname,
    String? password,
    SaslCredentials? sasl,
    int? retention,
    List<String>? altNicks,
    String? libraryPath,
  }) {
    return ObbyClient.fromConfig({
      'nick': nick,
      if (username != null) 'username': username,
      if (realname != null) 'realname': realname,
      if (password != null) 'password': password,
      if (sasl != null) 'sasl': sasl,
      if (retention != null) 'retention': retention,
      if (altNicks != null) 'alt_nicks': altNicks,
    }, libraryPath: libraryPath);
  }

  /// Open a connection's engine from a config map already in the wire shape.
  ///
  /// For a host that already has a config to hand, built or loaded some other way. Only `nick` is
  /// required in [config]; every other field has a default.
  factory ObbyClient.fromConfig(Map<String, dynamic> config, {String? libraryPath}) {
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
  ///
  /// An event's `type` names it, in `snake_case`, and the rest of the map is that event's fields.
  ///
  /// ```dart
  /// for (final event in client.pollEvents()) {
  ///   switch (event['type']) {
  ///     case 'registered':
  ///       print('registered as ${event['nick']}');
  ///     case 'model_changed':
  ///       print(event['change']);
  ///     case 'server_reply':
  ///       print('${event['severity']} ${event['code']} ${event['text']}');
  ///   }
  /// }
  /// ```
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

  /// Do something on this connection, as a command map the host built itself.
  ///
  /// Returns false when the engine could not read the map: an unknown `type`, a missing field, or a
  /// field of the wrong shape. Every named command below builds its own map, so only this one can
  /// be handed something unreadable.
  ///
  /// ```dart
  /// if (!client.command({'type': 'join', 'channel': '#obby', 'key': null})) {
  ///   print('the engine could not read that command');
  /// }
  /// ```
  bool command(Map<String, dynamic> command) {
    _alive();
    final json = jsonEncode(command).toNativeUtf8();
    try {
      return _bindings.command(_handle, json);
    } finally {
      calloc.free(json);
    }
  }

  /// Join a channel, with its key when it has one.
  ///
  /// ```dart
  /// client.join('#obby');
  /// client.join('#staff', key: 'hunter2');
  /// ```
  void join(String channel, {String? key}) {
    _alive();
    command({'type': 'join', 'channel': channel, 'key': key});
  }

  /// Leave a channel.
  void part(String channel, {String? reason}) {
    _alive();
    command({'type': 'part', 'channel': channel, 'reason': reason});
  }

  /// Say something to a channel or a person.
  ///
  /// ```dart
  /// client.sendMessage('#obby', 'hello there');
  /// client.sendMessage('alice', 'a private word');
  /// ```
  void sendMessage(String target, String text) {
    _alive();
    command({'type': 'send_message', 'target': target, 'text': text});
  }

  /// Send a notice, which by convention must never be auto-replied to.
  void sendNotice(String target, String text) {
    _alive();
    command({'type': 'send_notice', 'target': target, 'text': text});
  }

  /// Send a `CTCP ACTION`, the third-person form.
  void sendAction(String target, String text) {
    _alive();
    command({'type': 'send_action', 'target': target, 'text': text});
  }

  /// Change our nick.
  void setNick(String nick) {
    _alive();
    command({'type': 'set_nick', 'nick': nick});
  }

  /// Set or clear a channel topic.
  void setTopic(String channel, {String? topic}) {
    _alive();
    command({'type': 'set_topic', 'channel': channel, 'topic': topic});
  }

  /// Mark ourselves away, or come back.
  void setAway({String? message}) {
    _alive();
    command({'type': 'set_away', 'message': message});
  }

  /// Say we are typing, so others can show it.
  void setTyping(String target, TypingState state) {
    _alive();
    command({'type': 'set_typing', 'target': target, 'state': state._wire});
  }

  /// React to a message with an emoji.
  void addReaction(String target, String msgid, String emoji) {
    _alive();
    command({'type': 'add_reaction', 'target': target, 'msgid': msgid, 'emoji': emoji});
  }

  /// Take a reaction back.
  void removeReaction(String target, String msgid, String emoji) {
    _alive();
    command({'type': 'remove_reaction', 'target': target, 'msgid': msgid, 'emoji': emoji});
  }

  /// Ask the server to delete a message.
  void redactMessage(String target, String msgid, {String? reason}) {
    _alive();
    command({
      'type': 'redact_message',
      'target': target,
      'msgid': msgid,
      'reason': reason,
    });
  }

  /// Tell the server how far we have read, in milliseconds since the Unix epoch.
  void markRead(String target, int atMs) {
    _alive();
    command({'type': 'mark_read', 'target': target, 'at_ms': atMs});
  }

  /// Ask for older messages than the ones we hold.
  ///
  /// With no [beforeMsgid], this asks for the most recent, which is what a fresh window wants.
  void fetchHistory(String target, {String? beforeMsgid, int limit = 50}) {
    _alive();
    command({
      'type': 'fetch_history',
      'target': target,
      'before_msgid': beforeMsgid,
      'limit': limit,
    });
  }

  /// Set one of our own metadata keys, or clear it.
  void setMetadata(String key, {String? value}) {
    _alive();
    command({'type': 'set_metadata', 'key': key, 'value': value});
  }

  /// Ask to be told when these metadata keys change on anyone we can see.
  void subscribeMetadata(List<String> keys) {
    _alive();
    command({'type': 'subscribe_metadata', 'keys': keys});
  }

  /// Watch these nicks, so the server says when they come and go.
  void watchNicks(List<String> nicks) {
    _alive();
    command({'type': 'watch_nicks', 'nicks': nicks});
  }

  /// Stop watching these nicks.
  void unwatchNicks(List<String> nicks) {
    _alive();
    command({'type': 'unwatch_nicks', 'nicks': nicks});
  }

  /// Send a voice signalling frame to a room.
  ///
  /// The frame is the caller's to build: everything in it comes from a media stack the engine
  /// deliberately knows nothing about.
  void sendVoiceSignal(String channel, Map<String, dynamic> signal) {
    _alive();
    command({
      'type': 'send_voice_signal',
      'channel': channel,
      'signal': signal,
    });
  }

  /// Leave the network.
  void quit({String? reason}) {
    _alive();
    command({'type': 'quit', 'reason': reason});
  }

  /// Send a line the engine does not model. The escape hatch, so you are never stuck waiting on it.
  void sendRawLine(String line) {
    _alive();
    command({'type': 'send_raw_line', 'line': line});
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
