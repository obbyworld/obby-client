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
  void connected() {
    _alive();
    _bindings.connected(_handle);
  }

  /// Tell the engine its transport died. The model survives, so a reconnect resumes from it.
  void disconnected() {
    _alive();
    _bindings.disconnected(_handle);
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
