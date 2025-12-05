# Redlib Docker Images on GHCR

This fork publishes Docker images to GitHub Container Registry (GHCR) with the latest cipher suite fixes to avoid Reddit TLS fingerprinting.

## Quick Start

Pull and run the latest image:

```bash
docker pull ghcr.io/mitchross/redlib:latest
docker run -d -p 8080:8080 ghcr.io/mitchross/redlib:latest
```

Then visit http://localhost:8080 in your browser.

## Available Tags

- `latest` - Latest build from the main branch
- `main` - Latest build from the main branch  
- `sha-<commit>` - Specific commit builds
- `v*` - Version tagged releases

## Multi-Architecture Support

Images are built for the following architectures:
- `linux/amd64` (x86_64)
- `linux/arm64` (aarch64)
- `linux/arm/v7` (armv7)

Docker will automatically pull the correct image for your platform.

## Docker Compose

Example `docker-compose.yml`:

```yaml
services:
  redlib:
    image: ghcr.io/mitchross/redlib:latest
    ports:
      - "8080:8080"
    environment:
      - REDLIB_DEFAULT_THEME=dark
      - REDLIB_DEFAULT_FRONT_PAGE=popular
    restart: unless-stopped
```

## Environment Variables

See the main [README](../README.md) for available environment variables.

## What's Different?

This fork includes the cipher suite fix from [PR #510](https://github.com/redlib-org/redlib/pull/510) that resolves Reddit's TLS fingerprinting blocking. The fix changes the TLS cipher suites to match Firefox's configuration.

## Building Locally

To build the image yourself:

```bash
# For amd64
docker build -f Dockerfile.build --build-arg TARGET=x86_64-unknown-linux-musl -t redlib:local .

# For arm64
docker build -f Dockerfile.build --build-arg TARGET=aarch64-unknown-linux-musl -t redlib:local .

# For armv7
docker build -f Dockerfile.build --build-arg TARGET=armv7-unknown-linux-musleabihf -t redlib:local .
```

## Automated Builds

Images are automatically built and pushed to GHCR when:
- Code is pushed to the `main` branch
- A version tag (v*) is created
- Manual workflow dispatch is triggered

The workflow uses GitHub Actions with multi-platform builds via Docker Buildx.
