# Build environment for JeroMQ, the pure Java ZMTP implementation.
#
# No libzmq and no JNI: JeroMQ reimplements the protocol on the JVM. The image
# builds the wrapper and jlinks a minimal runtime, so the exported filesystem
# carries its own JVM and the bench host needs no JDK. See docs/CONTAINERS.md.
#
# JeroMQ 0.6.0 is from February 2024 and the project has been quiet since; it is
# still the de facto JVM implementation, and the version travels with every
# record so a reader can weigh that themselves.
FROM eclipse-temurin:21-jdk AS build

ARG JEROMQ_VERSION=0.6.0

WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && curl -sSfLo /src/jeromq.jar \
      "https://repo1.maven.org/maven2/org/zeromq/jeromq/${JEROMQ_VERSION}/jeromq-${JEROMQ_VERSION}.jar"

COPY src/Target.java /src/Target.java
RUN javac -cp /src/jeromq.jar -d /src/classes /src/Target.java

# A minimal runtime rather than a full JDK: the exported filesystem is shipped
# and a JDK would be several hundred MB of compiler nobody runs.
RUN jlink --add-modules java.base,jdk.unsupported,java.management \
      --strip-debug --no-header-files --no-man-pages --compress=zip-6 \
      --output /jre

FROM ubuntu:24.04
COPY --from=build /jre /app/jre
COPY --from=build /src/jeromq.jar /app/jeromq.jar
COPY --from=build /src/classes /app/classes
ARG JEROMQ_VERSION=0.6.0
# The launcher carries its own environment and absolute paths: `docker export`
# drops image metadata, so an ENV or PATH set here would not survive it.
#
# exec, so the process the orchestrator tracks is the JVM itself; without it the
# shell would be the child and the peak-RSS sampling would follow the wrapper.
#
# -XX:+AlwaysPreTouch is deliberately NOT set, and the heap is left at its
# default: pinning it would be a tuning decision, and tuning belongs to the
# library's maintainer. Note that peak RSS for this target therefore measures
# the JVM's default heap policy rather than the library's own appetite.
RUN printf '#!/bin/sh\nexec /app/jre/bin/java -Darena.jeromq.version=%s -cp /app/classes:/app/jeromq.jar Target "$@"\n' \
      "${JEROMQ_VERSION}" > /app/target \
 && chmod +x /app/target
RUN /app/target describe > /app/describe.json && cat /app/describe.json
