import tseslint from 'typescript-eslint';
export default tseslint.config(
  { ignores: ['src/api/generated/**'] },
  ...tseslint.configs.recommended,
);
