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
  },
};
