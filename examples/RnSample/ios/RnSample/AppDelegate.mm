#import "AppDelegate.h"

#import <React/RCTBundleURLProvider.h>
#import <React/RCTHTTPRequestHandler.h>

// Order matters: PqcCore's Swift-generated ObjC header MUST come
// before the app's auto-generated Swift header. `RnSample-Swift.h`
// (imported below) references `PqcURLProtocol` as the superclass of
// our bridge subclass `RnSamplePqcURLProtocol`; without `PqcURLProtocol`
// declared first the ObjC compiler emits:
//   "Cannot find interface declaration for 'PqcURLProtocol', superclass
//    of 'RnSamplePqcURLProtocol'".
//
// Quote-form `#import "PqcCore-Swift.h"` (no framework prefix, no
// `@import`) deliberately avoids the C++ modules requirement that
// `@import PqcCore;` would impose on this `.mm` (needs `-fcxx-modules`,
// which RN does not enable). The PqcCore podspec's `user_target_xcconfig`
// adds the Swift Compatibility Header directory to this app's
// HEADER_SEARCH_PATHS so the quote-form import resolves with zero
// consumer-side xcconfig or pbxproj changes.
#import "PqcCore-Swift.h"
#import "RnSample-Swift.h"

@implementation AppDelegate

- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)launchOptions
{
  self.moduleName = @"RnSample";
  self.initialProps = @{};

  // Route RN's fetch() / XHR through PqcURLProtocol. Must be installed
  // before any JS executes — RCTHTTPRequestHandler reads the provider
  // lazily on first request. The framework's `registerIfNeededInto:`
  // helper handles the iOS 26 gate (Apple's URLSession negotiates
  // X25519MLKEM768 natively from iOS 26, so the URLProtocol skips
  // there). Exposed via `@objc(registerIfNeededInto:)` on the
  // PqcURLProtocol class in v0.8.1+.
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
