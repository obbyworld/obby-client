# obby_client

Dart bindings for the Obby IRCv3 client engine. It parses the protocol, drives the connection and
holds the client model. It opens no socket and keeps no clock, so you hand it bytes and the time.

```sh
dart pub add obby_client
```

The engine is a native library reached through its C ABI, so your application ships `libobby_ffi`
and points the client at it. Every release includes a build for Linux, macOS and Windows:
<https://github.com/obbyworld/obby-client/releases>.

```dart
import 'package:obby_client/obby_client.dart';

final client = ObbyClient({'nick': 'mynick'}, libraryPath: 'libobby_ffi.so');
client.connected();

socket.listen((data) {
  client.handleBytes(data);
  for (final event in client.pollEvents()) render(event);
  for (var bytes = client.pollTransmit(); bytes != null; bytes = client.pollTransmit()) {
    socket.add(bytes);
  }
});

client.command({'command': 'join', 'channel': '#obby', 'key': null});
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
