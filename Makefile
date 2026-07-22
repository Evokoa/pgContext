EXTENSION := pgcontext
PACKAGE := context-pg
PG_CONFIG ?= pg_config
PGRX ?= cargo pgrx
TAG ?= v0.1.0
PG_MAJOR := $(shell $(PG_CONFIG) --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\1/')
PG_FEATURE := pg$(PG_MAJOR)

.PHONY: all check-pg17 install install-pgvector-bridge installcheck package quickstart clean

all: package

check-pg17:
	@test "$(PG_MAJOR)" = "17" || { \
		echo "pgContext source launch supports PostgreSQL 17; selected $(PG_MAJOR)" >&2; \
		exit 1; \
	}

install: check-pg17
	$(PGRX) install -p $(PACKAGE) --pg-config $(PG_CONFIG) --release \
		--no-default-features --features $(PG_FEATURE)
	$(MAKE) install-pgvector-bridge

install-pgvector-bridge: check-pg17
	scripts/install-pgvector-bridge.sh $(PG_CONFIG)

installcheck: check-pg17
	$(PGRX) test -p $(PACKAGE) $(PG_FEATURE)

package:
	release/build-packages.sh $(TAG)

quickstart:
	scripts/quickstart.sh

clean:
	cargo clean
	rm -rf dist
