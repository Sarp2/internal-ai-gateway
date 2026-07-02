import assert from 'node:assert/strict';
import type { Server } from 'node:http';
import { after, before, test } from 'node:test';
import { createProxyServer } from './server.ts';

let server: Server;
let baseUrl: string;

before(async () => {
	server = createProxyServer();

	await new Promise<void>((resolve) => {
		server.listen(0, '127.0.0.1', resolve);
	});

	const address = server.address();

	assert(address && typeof address === 'object');
	baseUrl = `http://${address.address}:${address.port}`;
});

after(async () => {
	await new Promise<void>((resolve, reject) => {
		server.close((error) => {
			if (error) {
				reject(error);
				return;
			}

			resolve();
		});
	});
});

test('returns healthy status from health route', async () => {
	const response = await fetch(`${baseUrl}/health`);

	assert.equal(response.status, 200);
	assert.deepEqual(await response.json(), { status: 'ok' });
});

test('returns not found for unknown routes', async () => {
	const response = await fetch(`${baseUrl}/unknown`);

	assert.equal(response.status, 404);
	assert.deepEqual(await response.json(), { message: 'Route not found.' });
});
