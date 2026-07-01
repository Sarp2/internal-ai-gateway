import type { StackProps } from 'aws-cdk-lib';
import { Stack } from 'aws-cdk-lib';
import { IpAddresses, SubnetType, Vpc } from 'aws-cdk-lib/aws-ec2';
import type { Construct } from 'constructs';

export class NetworkStack extends Stack {
	public readonly vpc: Vpc;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		this.vpc = new Vpc(this, 'GatewayVpc', {
			ipAddresses: IpAddresses.cidr('10.0.0.0/16'),
			maxAzs: 2,
			natGateways: 1,
			subnetConfiguration: [
				{
					name: 'public',
					subnetType: SubnetType.PUBLIC,
					cidrMask: 24,
				},
				{
					name: 'private',
					subnetType: SubnetType.PRIVATE_WITH_EGRESS,
					cidrMask: 24,
				},
			],
		});
	}
}
