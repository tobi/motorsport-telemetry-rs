PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

CARGO ?= cargo
BIN := motorsport-telemetry

.PHONY: build install uninstall clean

build:
	$(CARGO) build --release --bin $(BIN)

install: build
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "target/release/$(BIN)" "$(DESTDIR)$(BINDIR)/$(BIN)"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(BIN)"

clean:
	$(CARGO) clean
