import type { StackProps } from 'aws-cdk-lib';
import { RemovalPolicy, Stack } from 'aws-cdk-lib';
import { BlockPublicAccess, Bucket, BucketEncryption } from 'aws-cdk-lib/aws-s3';
import type { Construct } from 'constructs';

export class S3Stack extends Stack {
	public readonly messagesBucket: Bucket;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		this.messagesBucket = new Bucket(this, 'MessagesBucket', {
			blockPublicAccess: BlockPublicAccess.BLOCK_ALL,
			encryption: BucketEncryption.S3_MANAGED,
			enforceSSL: true,
			removalPolicy: RemovalPolicy.RETAIN,
		});
	}
}
