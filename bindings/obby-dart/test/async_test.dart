import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:obby_client/obby_client.dart';
import 'package:obby_client/obby_client_async.dart';
import 'package:test/test.dart';

void main() {
  test('connecting queues the registration burst onto the fake socket', () {
    final incoming = StreamController<List<int>>();
    final sent = <String>[];
    final driver = ObbyAsyncClient(incoming.stream, (data) => sent.add(utf8.decode(data)), nick: 'me');
    addTearDown(() async {
      await driver.close();
      await incoming.close();
    });

    expect(sent.join(), startsWith('CAP LS 302'));
    expect(sent.join(), contains('NICK me'));
  });

  test('a 001 fed into the incoming stream produces a registered event', () async {
    final incoming = StreamController<List<int>>();
    final driver = ObbyAsyncClient(incoming.stream, (_) {}, nick: 'me');
    addTearDown(() async {
      await driver.close();
      await incoming.close();
    });

    final firstEvent = driver.events.first;
    incoming.add(utf8.encode(':server 001 me :Welcome\r\n'));

    final event = await firstEvent;
    expect(event, isA<ObbyEventRegistered>());
    expect((event as ObbyEventRegistered).nick, 'me');
  });

  test('a chunk arriving after the transport errored is dropped', () async {
    final incoming = StreamController<List<int>>();
    final driver = ObbyAsyncClient(incoming.stream, (_) {}, nick: 'me');
    addTearDown(driver.close);

    incoming.addError(const SocketException('the link died'));
    await pumpEventQueue();
    incoming.add(utf8.encode(':server 001 me :Welcome\r\n'));
    await pumpEventQueue();

    expect(driver.client.version, isNotEmpty, reason: 'a late chunk must not reach a dead driver');
  });

  test('the transport closing ends the event stream without closing the client', () async {
    final incoming = StreamController<List<int>>();
    final driver = ObbyAsyncClient(incoming.stream, (_) {}, nick: 'me');
    addTearDown(driver.close);

    final done = driver.events.isEmpty;
    await incoming.close();

    expect(await done, isTrue);
    expect(driver.client.version, isNotEmpty, reason: 'the engine survives a dead transport');
  });
}
