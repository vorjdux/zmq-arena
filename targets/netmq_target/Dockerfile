# Build environment for NetMQ, the pure C# ZMTP implementation.
#
# Published self-contained, so the exported filesystem carries its own .NET
# runtime and needs nothing installed on the bench host. The image pins the SDK;
# it is not a runtime. See docs/CONTAINERS.md.
FROM mcr.microsoft.com/dotnet/sdk:8.0 AS build

WORKDIR /src
COPY netmq-target.csproj /src/
RUN dotnet restore
COPY Program.cs /src/
# Self-contained rather than framework-dependent: a framework-dependent binary
# would need a .NET runtime present at the new root, and the point of exporting
# the filesystem is that the host has nothing installed. Single file keeps the
# launcher honest, since the process the orchestrator tracks is the binary
# itself rather than a host `dotnet` that would exec it.
RUN dotnet publish -c Release -r linux-x64 --self-contained true \
      -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true \
      -o /out

FROM ubuntu:24.04
# The self-contained publish still links against libc and ICU-less globalization
# is enabled in the csproj, so only the base libraries are needed here.
COPY --from=build /out/netmq-target /app/target
RUN chmod +x /app/target
# No ENV is set anywhere: it would not survive `docker export`, which flattens
# the filesystem and drops image metadata. A self-contained binary needs none.
RUN /app/target describe > /app/describe.json && cat /app/describe.json
