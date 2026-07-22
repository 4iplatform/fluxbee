# Fluxbee open-source site

Static, dependency-free site for the public Fluxbee repository. It is separate
from `docs/website/`, which contains the commercial `fluxbee.ai` website.

## Preview locally

```bash
python3 -m http.server 4173 --directory opensource-site
```

Then open `http://localhost:4173`.

## Publish

The workflow at `.github/workflows/pages.yml` deploys this directory. In the
repository settings, select **Settings → Pages → Build and deployment → GitHub
Actions** once. After that, pushes to `main` that change this directory deploy
automatically. The workflow can also be run manually.

All links and assets are relative so the site works at both a GitHub project
subpath and a custom domain.
