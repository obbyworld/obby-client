# obby_client

**Write the interface. This handles IRC.**

An IRCv3 engine with the client model built in, for Dart and Flutter. It parses the protocol,
negotiates capabilities, authenticates, and keeps channels, members, conversations and their
messages. It does no I/O: you feed it bytes and the time, it tells you what happened and what to
send.

```sh
dart pub add obby_client
```

The engine is a native library reached through its C ABI, so your application ships `libobby_ffi`
and points the client at it. Every release includes a build for Linux, macOS and Windows:
<https://github.com/obbyworld/obby-client/releases>.

```dart
import 'package:obby_client/obby_client.dart';

final client = ObbyClient(nick: 'mynick', libraryPath: 'libobby_ffi.so');
client.handleConnected();

socket.listen((data) {
  client.handleBytes(data);
  for (final event in client.pollEvents()) render(event);
  for (var bytes = client.pollTransmit(); bytes != null; bytes = client.pollTransmit()) {
    socket.add(bytes);
  }
});

client.join('#obby');
client.close();
```

Call `close` when finished. The engine holds native memory that Dart's collector knows nothing
about. One client belongs to one isolate: the native handle is not synchronised, so give each
isolate its own.

## Build from source

```sh
cargo build -p obby-ffi --release
dart pub get && dart test
```
