/// An async driver for [ObbyClient], giving a host a `Stream<ObbyEvent>` and owning the poll/drain
/// loop for it.
///
/// A separate library from `obby_client.dart` on purpose: the sans-io engine and its manual loop
/// are untouched and keep working.
library;

import 'dart:async';
import 'dart:typed_data';

import 'obby_client.dart';

/// Drives an [ObbyClient] off an already-open transport: bytes in through [incoming], bytes out
/// through [send]. A `dart:io` `Socket` supplies both: pass the socket itself as `incoming` and
/// `socket.add` as `send`. A `WebSocket`, or any other `Stream<List<int>>` with a matching sink,
/// fits the same way.
///
/// Owns the same poll/drain loop the manual example in `obby_client.dart` does: it flushes
/// `pollTransmit` after every input, ticks the clock on every input, and schedules the next `tick`
/// from `pollTimeout()` with a [Timer], so the loop sleeps until the engine has something to do.
///
/// Call [close] when finished, same as with a plain [ObbyClient]: it cancels the timer, stops
/// listening, closes [events], and frees the engine.
///
/// ```dart
/// final socket = await Socket.connect('irc.libera.chat', 6667);
/// final driver = ObbyAsyncClient(socket, socket.add, nick: 'mynick');
///
/// await for (final event in driver.events) {
///   if (event is ObbyEventRegistered) driver.client.join('#obby');
/// }
/// ```
class ObbyAsyncClient {
  /// Build the engine and start driving it over [incoming]/[send]. Only [nick] is required; every
  /// other field has the same default [ObbyClient.new] does.
  ObbyAsyncClient(
    Stream<List<int>> incoming,
    void Function(List<int> data) send, {
    required String nick,
    String? username,
    String? realname,
    String? password,
    SaslCredentialsInput? sasl,
    int? retention,
    List<String>? altNicks,
    String? libraryPath,
  }) : this.withClient(
         ObbyClient(
           nick: nick,
           username: username,
           realname: realname,
           password: password,
           sasl: sasl,
           retention: retention,
           altNicks: altNicks,
           libraryPath: libraryPath,
         ),
         incoming,
         send,
       );

  /// Drive an [ObbyClient] a host already built, over an already-open transport.
  ObbyAsyncClient.withClient(this.client, Stream<List<int>> incoming, this._send) {
    _stopwatch.start();
    client.handleConnected();
    _flush();
    _scheduleTick();
    _sub = incoming.listen(
      (chunk) {
        // an errored stream keeps delivering unless it is told not to, and the engine is gone by
        // then
        if (_closed) return;
        client.handleBytes(Uint8List.fromList(chunk));
        _tickNow();
        _flush();
        _drain();
        _scheduleTick();
      },
      onDone: _onTransportClosed,
      onError: (Object _) => _onTransportClosed(),
      cancelOnError: true,
    );
  }

  /// The underlying engine, for commands the typed helpers on it don't cover.
  final ObbyClient client;

  final void Function(List<int> data) _send;
  final _stopwatch = Stopwatch();
  // broadcast, so `close()` completes even for a host that never listens to `events`: a
  // single-subscription controller's `close()` future only resolves once something has listened
  final _events = StreamController<ObbyEvent>.broadcast();
  StreamSubscription<List<int>>? _sub;
  Timer? _timer;
  bool _closed = false;

  /// Every event the engine produces, decoded as it arrives. Ends once the transport closes.
  Stream<ObbyEvent> get events => _events.stream;

  void _tickNow() {
    client.tick(_stopwatch.elapsedMilliseconds, DateTime.now().millisecondsSinceEpoch);
  }

  void _flush() {
    for (var bytes = client.pollTransmit(); bytes != null; bytes = client.pollTransmit()) {
      _send(bytes);
    }
  }

  void _drain() {
    if (_events.isClosed) return;
    for (final event in client.pollEvents()) {
      _events.add(event);
    }
  }

  void _scheduleTick() {
    _timer?.cancel();
    if (_closed) return;
    final deadline = client.pollTimeout();
    if (deadline == null) return;
    final remaining = deadline - _stopwatch.elapsedMilliseconds;
    _timer = Timer(Duration(milliseconds: remaining > 0 ? remaining : 0), () {
      _tickNow();
      _flush();
      _drain();
      _scheduleTick();
    });
  }

  void _onTransportClosed() {
    if (_closed) return;
    _closed = true;
    client.handleDisconnected();
    _timer?.cancel();
    unawaited(_events.close());
  }

  /// Close cleanly: cancel the timer, stop listening, close [events], and free the engine.
  ///
  /// Safe to call after the transport has already closed on its own: that only ends [events] and
  /// tells the engine it is disconnected, since the engine's model survives a dead link on purpose
  /// so a reconnect can resume from it. This is what actually releases the native handle.
  Future<void> close() async {
    _closed = true;
    _timer?.cancel();
    await _sub?.cancel();
    await _events.close();
    client.close();
  }
}
