import 'dart:ffi';

import 'package:ffi/ffi.dart';

/// An owned byte buffer the engine handed out.
///
/// Mirrors `obby_bytes_t`. Released only with `obby_client_free_bytes`, never with `free`.
final class ObbyBytes extends Struct {
  external Pointer<Uint8> ptr;

  @Size()
  external int len;
}

/// The opaque engine handle. Mirrors `obby_client_t`.
final class ObbyClientHandle extends Opaque {}

typedef _NewNative = Pointer<ObbyClientHandle> Function(Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<ObbyClientHandle>);
typedef _Free = void Function(Pointer<ObbyClientHandle>);
typedef _VoidOnClientNative = Void Function(Pointer<ObbyClientHandle>);
typedef _VoidOnClient = void Function(Pointer<ObbyClientHandle>);
typedef _HandleBytesNative =
    Void Function(Pointer<ObbyClientHandle>, Pointer<Uint8>, Size);
typedef _HandleBytes =
    void Function(Pointer<ObbyClientHandle>, Pointer<Uint8>, int);
typedef _PollTransmitNative = ObbyBytes Function(Pointer<ObbyClientHandle>);
typedef _FreeBytesNative = Void Function(ObbyBytes);
typedef _FreeBytes = void Function(ObbyBytes);
typedef _StringOnClientNative = Pointer<Utf8> Function(Pointer<ObbyClientHandle>);
typedef _FreeStringNative = Void Function(Pointer<Utf8>);
typedef _FreeString = void Function(Pointer<Utf8>);
typedef _TickNative =
    Void Function(Pointer<ObbyClientHandle>, Uint64, Uint64);
typedef _Tick = void Function(Pointer<ObbyClientHandle>, int, int);
typedef _PollTimeoutNative =
    Bool Function(Pointer<ObbyClientHandle>, Pointer<Uint64>);
typedef _PollTimeout =
    bool Function(Pointer<ObbyClientHandle>, Pointer<Uint64>);
typedef _CommandNative = Bool Function(Pointer<ObbyClientHandle>, Pointer<Utf8>);
typedef _Command = bool Function(Pointer<ObbyClientHandle>, Pointer<Utf8>);
typedef _VersionNative = Pointer<Utf8> Function();

/// The C functions, looked up once.
///
/// Kept apart from the Dart API in `client.dart` so the unsafe surface is one file, mirroring how
/// the Rust side keeps all its `unsafe` in `obby-ffi` alone.
class Bindings {
  Bindings(DynamicLibrary library)
    : newClient = library.lookupFunction<_NewNative, _NewNative>(
        'obby_client_new_from_json',
      ),
      freeClient = library.lookupFunction<_FreeNative, _Free>(
        'obby_client_free',
      ),
      connected = library.lookupFunction<_VoidOnClientNative, _VoidOnClient>(
        'obby_client_connected',
      ),
      disconnected = library.lookupFunction<_VoidOnClientNative, _VoidOnClient>(
        'obby_client_disconnected',
      ),
      handleBytes = library.lookupFunction<_HandleBytesNative, _HandleBytes>(
        'obby_client_handle_bytes',
      ),
      pollTransmit = library
          .lookupFunction<_PollTransmitNative, _PollTransmitNative>(
            'obby_client_poll_transmit',
          ),
      freeBytes = library.lookupFunction<_FreeBytesNative, _FreeBytes>(
        'obby_client_free_bytes',
      ),
      pollEvents = library
          .lookupFunction<_StringOnClientNative, _StringOnClientNative>(
            'obby_client_poll_events_json',
          ),
      modelJson = library
          .lookupFunction<_StringOnClientNative, _StringOnClientNative>(
            'obby_client_model_json',
          ),
      freeString = library.lookupFunction<_FreeStringNative, _FreeString>(
        'obby_client_free_string',
      ),
      tick = library.lookupFunction<_TickNative, _Tick>('obby_client_tick'),
      pollTimeout = library.lookupFunction<_PollTimeoutNative, _PollTimeout>(
        'obby_client_poll_timeout',
      ),
      command = library.lookupFunction<_CommandNative, _Command>(
        'obby_client_command_from_json',
      ),
      version = library.lookupFunction<_VersionNative, _VersionNative>(
        'obby_client_version',
      );

  final Pointer<ObbyClientHandle> Function(Pointer<Utf8>) newClient;
  final void Function(Pointer<ObbyClientHandle>) freeClient;
  final void Function(Pointer<ObbyClientHandle>) connected;
  final void Function(Pointer<ObbyClientHandle>) disconnected;
  final void Function(Pointer<ObbyClientHandle>, Pointer<Uint8>, int)
  handleBytes;
  final ObbyBytes Function(Pointer<ObbyClientHandle>) pollTransmit;
  final void Function(ObbyBytes) freeBytes;
  final Pointer<Utf8> Function(Pointer<ObbyClientHandle>) pollEvents;
  final Pointer<Utf8> Function(Pointer<ObbyClientHandle>) modelJson;
  final void Function(Pointer<Utf8>) freeString;
  final void Function(Pointer<ObbyClientHandle>, int, int) tick;
  final bool Function(Pointer<ObbyClientHandle>, Pointer<Uint64>) pollTimeout;
  final bool Function(Pointer<ObbyClientHandle>, Pointer<Utf8>) command;
  final Pointer<Utf8> Function() version;
}
