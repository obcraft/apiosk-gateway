import legacy from './legacy-executor/knip.json';

export default {
  treatConfigHintsAsErrors: true,
  // Fly and psql are system CLIs, not npm packages.
  ignoreBinaries: ['fly', 'psql'],
  workspaces: {
    '.': {
      entry: ['scripts/*.mjs', 'tests/*.mjs'],
      project: ['scripts/**/*.{js,mjs,cjs,ts}', 'tests/**/*.{js,mjs,cjs,ts}'],
      includeEntryExports: true,
    },
    ...Object.fromEntries(Object.entries(legacy.workspaces).map(([path, config]) => [
      path === '.' ? 'legacy-executor' : `legacy-executor/${path}`,
      // Standalone Knip discovers this CLI through legacy's GitHub Actions.
      // A check from the Gateway root needs the entry explicitly.
      path === '.' ? { ...config, entry: [...config.entry, 'scripts/route-monitor/monitor.mjs'] } : config,
    ])),
  },
};
