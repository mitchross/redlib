# Deployment Checklist

## ✅ Pre-Merge Verification

- [x] Code compiles successfully (`cargo check`)
- [x] All files properly committed
- [x] Documentation complete (README.md, DOCKER.md, PR_SUMMARY.md)
- [x] Code review feedback addressed
- [x] Clean commit history
- [x] No sensitive information in code

## 📋 Post-Merge Actions

### 1. Merge the PR
Once you merge this PR to the `main` branch:
- The GitHub Actions workflow will automatically trigger
- Docker images will be built for all architectures
- Images will be published to GHCR

### 2. Verify the Workflow
After merging, check:
1. Go to Actions tab in your GitHub repository
2. Look for "Build and Push to GHCR" workflow
3. Verify all builds complete successfully
4. Check for green checkmarks on all architecture builds

### 3. Verify GHCR Packages
Once the workflow completes:
1. Go to your repository page
2. Look for "Packages" section on the right sidebar
3. You should see `redlib` package listed
4. Click on it to see available tags

### 4. Test the Docker Image
Pull and test the image:
```bash
# Pull the latest image
docker pull ghcr.io/mitchross/redlib:latest

# Run it
docker run -d -p 8080:8080 --name redlib-test ghcr.io/mitchross/redlib:latest

# Wait a few seconds for startup
sleep 5

# Test it
curl http://localhost:8080/settings

# Clean up
docker stop redlib-test
docker rm redlib-test
```

### 5. Verify Reddit Access
Test that the cipher suite fix works:
```bash
docker run -d -p 8080:8080 --name redlib ghcr.io/mitchross/redlib:latest
```

Then open your browser to:
- http://localhost:8080/r/popular
- http://localhost:8080/r/all

You should NOT see "Failed to parse page JSON data" errors.

## 🔧 Making Changes

### To Update the Image
Simply push to main branch:
```bash
git checkout main
# make your changes
git add .
git commit -m "Your changes"
git push origin main
```

The workflow will automatically rebuild and publish new images.

### To Create a Version Release
Create and push a version tag:
```bash
git tag -a v0.36.1 -m "Release v0.36.1"
git push origin v0.36.1
```

This will create images tagged as:
- `v0.36.1`
- `v0.36`
- `v0`

## 📊 Monitoring

### Check Workflow Status
- Actions: https://github.com/mitchross/redlib/actions
- Workflows: https://github.com/mitchross/redlib/actions/workflows/ghcr.yml

### Check Package
- Packages: https://github.com/mitchross/redlib/pkgs/container/redlib

### Download Statistics
GitHub provides download statistics for packages in the package settings.

## 🐛 Troubleshooting

### Build Fails
If the workflow fails:
1. Check the workflow logs in the Actions tab
2. Look for specific error messages
3. Common issues:
   - Missing secrets (GITHUB_TOKEN is automatic)
   - Package permissions (check repository settings)
   - Build errors (check Rust code compiles locally)

### Image Won't Pull
If users can't pull the image:
1. Verify package visibility is set to "Public"
2. Go to package settings and ensure it's not private
3. Check that the image was published successfully

### Still Getting Rate Limited
If Reddit still blocks requests:
1. Verify the cipher suite fix was applied (check src/client.rs)
2. Consider using Tor/VPN (see upstream documentation)
3. Check if Reddit has changed their blocking mechanism

## 📝 Notes

- Images are automatically cleaned up after 30 days if untagged
- `latest` tag always points to the most recent main branch build
- Multi-arch manifests are created automatically
- Build cache is preserved between runs for faster builds

## 🎉 Success Criteria

Your deployment is successful when:
- [ ] GitHub Actions workflow completes without errors
- [ ] Package appears in GitHub Packages
- [ ] You can pull the image: `docker pull ghcr.io/mitchross/redlib:latest`
- [ ] The container starts successfully
- [ ] Reddit content loads without "Failed to parse JSON" errors
- [ ] All three architectures are available (amd64, arm64, armv7)

## 📚 Additional Resources

- [GitHub Packages Documentation](https://docs.github.com/en/packages)
- [Docker Multi-Platform Images](https://docs.docker.com/build/building/multi-platform/)
- [GitHub Actions Docker Build](https://docs.docker.com/build/ci/github-actions/)
