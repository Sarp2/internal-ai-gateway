import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import { NetworkStack } from './network-stack.ts';

test('defines a VPC across two availability zones', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::EC2::VPC', 1);
	template.resourceCountIs('AWS::EC2::Subnet', 4);
});

test('defines public subnets for internet-facing entrypoints', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::EC2::InternetGateway', 1);
	template.hasResourceProperties('AWS::EC2::Subnet', {
		MapPublicIpOnLaunch: true,
	});
});

test('defines private subnets with one NAT gateway for outbound egress', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::EC2::NatGateway', 1);
	template.resourceCountIs('AWS::EC2::EIP', 1);
	template.hasResourceProperties('AWS::EC2::Route', {
		DestinationCidrBlock: '0.0.0.0/0',
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new NetworkStack(app, 'TestNetworkStack');

	return Template.fromStack(stack);
}
