#import "AppDelegate.h"

#import <React/RCTBundleURLProvider.h>
#import <React/RCTHTTPRequestHandler.h>

// RnSamplePqcURLProtocol is a Swift subclass, @objc-visible via the
// auto-generated module header.
#import "RnSample-Swift.h"

@implementation AppDelegate

- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)launchOptions
{
  self.moduleName = @"RnSample";
  self.initialProps = @{};

  // Route RN's fetch() / XHR through PqcURLProtocol. Must be installed
  // before any JS executes — RCTHTTPRequestHandler reads the provider
  // lazily on first request. iOS 26+ negotiates X25519MLKEM768 natively,
  // so skip there.
  RCTSetCustomNSURLSessionConfigurationProvider(^NSURLSessionConfiguration *{
    NSURLSessionConfiguration *cfg = [NSURLSessionConfiguration defaultSessionConfiguration];
    if (@available(iOS 26.0, *)) {
      // native PQC
    } else {
      NSMutableArray *protocols = [NSMutableArray arrayWithObject:[RnSamplePqcURLProtocol class]];
      if (cfg.protocolClasses) {
        [protocols addObjectsFromArray:cfg.protocolClasses];
      }
      cfg.protocolClasses = protocols;
    }
    return cfg;
  });

  return [super application:application didFinishLaunchingWithOptions:launchOptions];
}

- (NSURL *)sourceURLForBridge:(RCTBridge *)bridge
{
  return [self bundleURL];
}

- (NSURL *)bundleURL
{
#if DEBUG
  return [[RCTBundleURLProvider sharedSettings] jsBundleURLForBundleRoot:@"index"];
#else
  return [[NSBundle mainBundle] URLForResource:@"main" withExtension:@"jsbundle"];
#endif
}

@end
