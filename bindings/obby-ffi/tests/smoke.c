/* A C program that drives the whole typed surface.
 *
 * Rust tests exercise these functions as Rust. This one proves the generated header compiles as C
 * and that a C caller reaches every part of the engine without touching JSON. */

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "obby_ffi.h"

/* One call hands over one buffer, so everything queued so far is read in a loop. */
static void expect_sent(obby_client_t *client, const char *needle) {
    char sent[8192] = {0};
    size_t used = 0;
    for (obby_bytes_t out = obby_client_poll_transmit(client); out.len > 0;
         out = obby_client_poll_transmit(client)) {
        assert(used + out.len < sizeof sent);
        memcpy(sent + used, out.ptr, out.len);
        used += out.len;
        obby_client_free_bytes(out);
    }
    assert(strstr(sent, needle) != NULL);
}

int main(void) {
    obby_config_t config = {.nick = "ctest", .retention = 50};
    obby_client_t *client = obby_client_new(&config);
    assert(client != NULL);

    obby_client_connected(client);
    expect_sent(client, "CAP LS 302");

    const char *welcome = ":server 001 ctest :Welcome\r\n";
    obby_client_handle_bytes(client, (const uint8_t *)welcome, strlen(welcome));

    obby_event_t *event = obby_client_poll_event(client);
    assert(event != NULL);
    assert(obby_event_get_kind(event) == OBBY_EVENT_KIND_REGISTERED);
    assert(strcmp(obby_event_text(event, OBBY_EVENT_FIELD_NICK), "ctest") == 0);
    assert(obby_event_text(event, OBBY_EVENT_FIELD_ACCOUNT) == NULL);
    assert(obby_event_json(event) != NULL);
    obby_event_free(event);

    assert(obby_client_join(client, "#obby", NULL));
    expect_sent(client, "JOIN #obby");

    assert(obby_client_send_message(client, "#obby", "hello from C"));
    expect_sent(client, "PRIVMSG #obby :hello from C");

    assert(obby_client_quit(client, "bye"));
    expect_sent(client, "QUIT bye");

    uint64_t timeout = 0;
    obby_client_poll_timeout(client, &timeout);

    obby_client_free(client);
    printf("obby-client %s: the C surface works\n", obby_client_version());
    return 0;
}
