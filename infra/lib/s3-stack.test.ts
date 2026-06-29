import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { S3Stack } from './s3-stack.ts';

test('defines the messages bucket with encryption and public access blocked', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::S3::Bucket', 1);
	template.hasResourceProperties('AWS::S3::Bucket', {
		BucketEncryption: {
			ServerSideEncryptionConfiguration: [
				{
					ServerSideEncryptionByDefault: {
						SSEAlgorithm: 'AES256',
					},
				},
			],
		},
		PublicAccessBlockConfiguration: {
			BlockPublicAcls: true,
			BlockPublicPolicy: true,
			IgnorePublicAcls: true,
			RestrictPublicBuckets: true,
		},
	});
});

test('retains the messages bucket when the stack is deleted', () => {
	const template = synthesizeTemplate();
	const buckets = template.findResources('AWS::S3::Bucket');

	for (const bucket of Object.values(buckets)) {
		strictEqual(bucket.DeletionPolicy, 'Retain');
		strictEqual(bucket.UpdateReplacePolicy, 'Retain');
	}
});

test('denies non-SSL access to the messages bucket', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::S3::BucketPolicy', {
		PolicyDocument: {
			Statement: Match.arrayWith([
				Match.objectLike({
					Effect: 'Deny',
					Principal: { AWS: '*' },
					Action: 's3:*',
					Condition: {
						Bool: {
							'aws:SecureTransport': 'false',
						},
					},
				}),
			]),
		},
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new S3Stack(app, 'TestS3Stack');

	return Template.fromStack(stack);
}
