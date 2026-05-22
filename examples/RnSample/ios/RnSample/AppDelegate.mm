#import "AppDelegate.h"

#import <React/RCTBundleURLProvider.h>
#import <React/RCTHTTPRequestHandler.h>

// PqcURLProtocol is a Swift class marked @objc; available through the
// auto-generated module header for the app target.
#import "RnSample-Swift.h"

@implementation AppDelegate

- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)launchOptions
{
  self.moduleName = @"RnSample";
  self.initialProps = @{};

  // Route React Native's fetch() / XHR through PqcURLProtocol so the TLS
  // handshake uses the Rust core (rustls + rustls-post-quantum). Must be
  // installed before any JS executes — RCTSetCustomNSURLSessionConfigurationProvider
  // is read lazily by RCTHTTPRequestHandler on first request.
  //
  // iOS 26+ already negotiates X25519MLKEM768 natively via URLSession, so we
  // skip registration there and let the system stack handle PQC.
  RCTSetCustomNSURLSessionConfigurationProvider(^NSURLSessionConfiguration *{
    NSURLSessionConfiguration *cfg = [NSURLSessionConfiguration defaultSessionConfiguration];
    if (@available(iOS 26.0, *)) {
      // native PQC — leave URLProtocol classes alone
    } else {
      NSMutableArray *protocols = [NSMutableArray arrayWithObject:[PqcURLProtocol class]];
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
