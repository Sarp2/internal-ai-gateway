# internal-ai-gateway

## Development

Install dependencies:

```bash
pnpm install
```

Run lint and format checks:

```bash
pnpm lint
```

Run TypeScript checks:

```bash
pnpm check
```

Run tests:

```bash
pnpm test
```

Synthesize the CDK app:

```bash
pnpm synth
```

## Integration Tests

Deploy the isolated integration environment, run its Rust test suite, and destroy the environment:

```bash
pnpm integration
```

Run each step separately when the environment needs to remain available for debugging:

```bash
pnpm integration:deploy
pnpm test:integration
pnpm integration:destroy
```

The deploy command writes stack discovery values to `.integration/cdk-outputs.json`. The complete
`pnpm integration` lifecycle always attempts teardown, including when deployment or tests fail.
