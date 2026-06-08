#import "AppDelegate.h"

#import <React/RCTBundleURLProvider.h>
#import <React/RCTHTTPRequestHandler.h>

// PqcCore-Swift.h must precede RnSample-Swift.h — the latter references
// PqcURLProtocol as the superclass of RnSamplePqcURLProtocol. See
// docs/ios.md §6 for the quote-form import rationale.
#import "PqcCore-Swift.h"
#import "RnSample-Swift.h"

@implementation AppDelegate

- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)launchOptions
{
  self.moduleName = @"RnSample";
  self.initialProps = @{};

  // Route RN's fetch() / XHR through PqcURLProtocol. registerIfNeededInto:
  // no-ops on iOS 26+ (native PQC).
  RCTSetCustomNSURLSessionConfigurationProvider(^NSURLSessionConfiguration *{
    NSURLSessionConfiguration *cfg = [NSURLSessionConfiguration defaultSessionConfiguration];
    [RnSamplePqcURLProtocol registerIfNeededInto:cfg];
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
