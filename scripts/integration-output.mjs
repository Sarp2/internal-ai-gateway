import { mkdir, rm } from 'node:fs/promises';

export async function prepareIntegrationOutput(outputDirectory, outputsFile) {
	await mkdir(outputDirectory, { recursive: true });
	await rm(outputsFile, { force: true });
}
