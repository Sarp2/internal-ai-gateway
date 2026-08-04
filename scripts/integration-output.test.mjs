import assert from 'node:assert/strict';
import { access, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { prepareIntegrationOutput } from './integration-output.mjs';

test('removes stale outputs before integration deployment', async () => {
	const testDirectory = await mkdtemp(join(tmpdir(), 'integration-output-'));
	const outputDirectory = join(testDirectory, '.integration');
	const outputsFile = join(outputDirectory, 'cdk-outputs.json');

	try {
		await prepareIntegrationOutput(outputDirectory, outputsFile);
		await writeFile(outputsFile, '{"stale":true}');

		await prepareIntegrationOutput(outputDirectory, outputsFile);

		await assert.rejects(access(outputsFile), { code: 'ENOENT' });
	} finally {
		await rm(testDirectory, { recursive: true, force: true });
	}
});
