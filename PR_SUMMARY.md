# Pull Request Summary

## Overview
This PR applies the cipher suite fix from the upstream repository to resolve Reddit's TLS fingerprinting blocking, and sets up automated Docker image publishing to GitHub Container Registry (GHCR).

## Problem Statement
As documented in [redlib-org/redlib#446](https://github.com/redlib-org/redlib/issues/446), Reddit has implemented TLS fingerprinting to block requests from redlib instances. The solution involves matching Firefox's cipher suite configuration to avoid detection.

## Changes Implemented

### 1. Cipher Suite Fix
**Files Modified:**
- `Cargo.toml` - Added `rustls = "0.21.12"` dependency
- `src/client.rs` - Updated HTTPS connector configuration
- `Cargo.lock` - Updated with new dependencies

**Details:**
- Implemented the fix from [redlib-org/redlib#510](https://github.com/redlib-org/redlib/pull/510)
- Changed TLS cipher suites to match Firefox 145.0
- Reordered cipher suites to match browser behavior
- Prevents Reddit from fingerprinting and blocking requests

### 2. GitHub Actions Workflow for GHCR
**File Created:**
- `.github/workflows/ghcr.yml`

**Features:**
- Multi-architecture builds (linux/amd64, linux/arm64, linux/arm/v7)
- Automated publishing to GitHub Container Registry
- Triggers on:
  - Push to main branch
  - Version tags (v*)
  - Manual workflow dispatch
  - Pull requests (build only, no publish)
- Uses digest-based multi-platform image creation
- Implements build caching for faster builds

### 3. Build Infrastructure
**File Created:**
- `Dockerfile.build`

**Features:**
- Multi-stage Docker build
- Builds from source (not pre-built binaries)
- Supports all three architectures via build args
- Minimal runtime image based on Alpine Linux
- Non-root user for security
- Healthcheck configuration included

### 4. Documentation
**Files Created/Modified:**
- `DOCKER.md` - New comprehensive Docker guide
- `README.md` - Updated with fork information

**Content:**
- Quick start guide for Docker users
- Available image tags documentation
- Multi-architecture support details
- Docker Compose examples
- Build instructions
- Environment variable reference

## Testing Performed
- ✅ Code compiles successfully with `cargo check`
- ✅ Docker workflow syntax validated
- ✅ Code review feedback addressed
- ✅ Documentation reviewed for accuracy
- ✅ All commits follow conventional commit format

## Security Considerations
- Uses official Rust Alpine images for reproducible builds
- Non-root user in final container image
- Minimal runtime dependencies
- No unnecessary packages in final image
- Healthcheck configured for container orchestration
- HTTPS enforced with modern cipher suites
- All GitHub Actions pinned to secure versions (actions/download-artifact@v4.1.8+ to avoid CVE)

## How to Use

### Pull from GHCR
```bash
docker pull ghcr.io/mitchross/redlib:latest
docker run -d -p 8080:8080 ghcr.io/mitchross/redlib:latest
```

### Available Tags
- `latest` - Latest build from main branch
- `main` - Latest build from main branch
- `sha-<commit>` - Specific commit builds
- `v*` - Version tagged releases

### Multi-Architecture Support
Images are automatically built for:
- `linux/amd64` (x86_64)
- `linux/arm64` (aarch64)  
- `linux/arm/v7` (armv7)

Docker will automatically pull the correct image for your platform.

## Next Steps
Once merged to main:
1. The GitHub Actions workflow will automatically build and publish images to GHCR
2. Users can start using `ghcr.io/mitchross/redlib:latest`
3. Images will be updated on every push to main
4. Version tags can be created to publish stable releases

## Related Issues/PRs
- Upstream issue: [redlib-org/redlib#446](https://github.com/redlib-org/redlib/issues/446)
- Upstream fix PR: [redlib-org/redlib#510](https://github.com/redlib-org/redlib/pull/510)
- Alternative fork: [baalajimaestro/redlib](https://git.ptr.moe/baalajimaestro/redlib)

## Verification
To verify the fix works:
1. Pull the latest image from GHCR once published
2. Run the container
3. Try accessing Reddit content through the instance
4. Verify no "Failed to parse page JSON data" errors occur

## Notes
- The cipher suite fix is a workaround for Reddit's TLS fingerprinting
- Reddit may change their blocking mechanisms in the future
- The multi-arch builds ensure wide compatibility across different devices
- GHCR provides unlimited public image pulls
