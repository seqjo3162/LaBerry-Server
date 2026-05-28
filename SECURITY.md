# LaBerry-Server Security

## Security Checklist

### Before Committing

- [ ] No `.env` files in staging
- [ ] No `.key`, `.pem`, `.pfx`, `.crt` files in staging
- [ ] No `.db`, `.sqlite` files in staging
- [ ] No `.log` files in staging
- [ ] No hardcoded secrets (SECRET_KEY, JWT_SECRET, PASSWORD, etc.)
- [ ] No SSH private keys (id_rsa*)
- [ ] No database dumps

### Secret Management

1. **Never commit secrets** — use `.env` files (ignored by git)
2. **Use `.env.example`** as template for new environments
3. **Rotate secrets** immediately if accidentally committed
4. **Use `git filter-repo`** to remove secrets from history

### Pre-commit Hook

The project includes a pre-commit hook at `.git/hooks/pre-commit` that:

- Blocks `.key`, `.pem`, `.pfx`, `.crt` files
- Blocks `.env` files
- Blocks database files (`.db`, `.sqlite`)
- Blocks log files (`.log`)
- Blocks hardcoded secrets in code

### Git History Cleanup

If secrets were committed:

1. Run `scripts/clean-git-history.ps1`
2. Or use `git filter-repo` manually
3. Force push and notify all collaborators to re-clone

### Environment Variables

See [`.env.example`](.env.example) for required configuration.

### Reporting Security Issues

Please report security issues responsibly.
