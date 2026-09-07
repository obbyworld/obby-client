import 'dart:convert';
import 'dart:typed_data';

import 'package:obby_client/obby_client.dart';
import 'package:test/test.dart';

void main() {
  ObbyClient open() => ObbyClient({'nick': 'me'});

  test('only the nick is required to build a client', () {
    final client = open();
    addTearDown(client.close);
    expect(client.version, isNotEmpty);
  });

  test('connecting queues the registration burst', () {
    final client = open();
    addTearDown(client.close);
    client.handleConnected();

    final sent = <String>[];
    for (var chunk = client.pollTransmit(); chunk != null; chunk = client.pollTransmit()) {
      sent.add(utf8.decode(chunk));
    }
    expect(sent.first, startsWith('CAP LS 302'));
    expect(sent.join(), contains('NICK me'));
  });

  test('a ping is answered with the same token', () {
    final client = open();
    addTearDown(client.close);
    client.handleBytes(Uint8List.fromList(utf8.encode('PING :abc\r\n')));
    expect(utf8.decode(client.pollTransmit()!), 'PONG abc\r\n');
  });

  test('events drain as a batch and the model comes back as a map', () {
    final client = open();
    addTearDown(client.close);
    client.handleBytes(
      Uint8List.fromList(utf8.encode(':s 001 me :Welcome\r\n:s 005 me CHANTYPES=# :ok\r\n')),
    );

    final events = client.pollEvents();
    expect(events, isNotEmpty);
    expect(events.first['type'], 'registered');
    expect(
      client.pollEvents(),
      isEmpty,
      reason: 'a drain takes everything, so the next call has nothing left',
    );
    expect(client.model()['me'], isA<Map<String, dynamic>>());
  });

  test('a command goes out and a malformed one is refused', () {
    final client = open();
    addTearDown(client.close);
    while (client.pollTransmit() != null) {}

    expect(client.command({'type': 'nick', 'nick': 'other'}), isTrue);
    expect(utf8.decode(client.pollTransmit()!), 'NICK other\r\n');
    expect(
      client.command({'type': 'not_a_real_command'}),
      isFalse,
      reason: 'an unreadable command must be reported, not silently dropped',
    );
  });

  test('a timeout is reported once something is scheduled', () {
    final client = open();
    addTearDown(client.close);
    expect(client.pollTimeout(), isNull);
    client.handleConnected();
    expect(client.pollTimeout(), isNotNull);
  });

  test('using a closed client is an error rather than a crash', () {
    final client = open();
    client.close();
    client.close();
    expect(client.handleConnected, throwsStateError);
  });
}
